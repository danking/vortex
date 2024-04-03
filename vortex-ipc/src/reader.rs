use crate::flatbuffers::ipc::Message;
use crate::iter::{FallibleLendingIterator, FallibleLendingIteratorඞItem};
use arrow_buffer::Buffer;
use flatbuffers::root;
use nougat::gat;
use std::io;
use std::io::{BufReader, Read};
use vortex::array::composite::COMPOSITE_EXTENSIONS;
use vortex::serde::context::SerdeContext;
use vortex::serde::ArrayView;
use vortex_error::{vortex_err, VortexError, VortexResult};
use vortex_flatbuffers::{FlatBufferReader, ReadFlatBuffer};
use vortex_schema::{DType, DTypeSerdeContext};

#[allow(dead_code)]
pub struct StreamReader<R: Read> {
    read: R,

    pub(crate) ctx: SerdeContext,
    // Optionally take a projection?

    // Use replace to swap the scratch buffer.
    // std::mem::replace
    // We could use a cell to avoid the need for mutable borrow.
    scratch: Vec<u8>,
}

impl<R: Read> StreamReader<BufReader<R>> {
    pub fn try_new(read: R) -> VortexResult<Self> {
        Self::try_new_unbuffered(BufReader::new(read))
    }
}

impl<R: Read> StreamReader<R> {
    pub fn try_new_unbuffered(mut read: R) -> VortexResult<Self> {
        let mut msg_vec = Vec::new();
        let fb_msg = read
            .read_message::<Message>(&mut msg_vec)?
            .ok_or_else(|| vortex_err!(InvalidSerde: "Unexpected EOF reading IPC format"))?;
        let fb_ctx = fb_msg.header_as_context().ok_or_else(
            || vortex_err!(InvalidSerde: "Expected IPC Context as first message in stream"),
        )?;
        let ctx: SerdeContext = fb_ctx.try_into()?;

        Ok(Self {
            read,
            ctx,
            scratch: Vec::with_capacity(1024),
        })
    }
}

/// We implement a lending iterator here so that each StreamArrayChunkReader can be lent as
/// mutable to the caller. This is necessary because we need a mutable handle to the reader.
#[gat]
impl<R: Read> FallibleLendingIterator for StreamReader<R> {
    type Error = VortexError;
    type Item<'next> =  StreamArrayChunkReader<'next, R> where Self: 'next;

    fn next(&mut self) -> Result<Option<StreamArrayChunkReader<'_, R>>, Self::Error> {
        let mut fb_vec = Vec::new();
        let msg = self.read.read_message::<Message>(&mut fb_vec)?;
        if msg.is_none() {
            // End of the stream
            return Ok(None);
        }
        let msg = msg.unwrap();

        // FIXME(ngates): parse the schema?
        let schema = msg
            .header_as_schema()
            .ok_or_else(|| vortex_err!(InvalidSerde: "Expected IPC Schema message"))?;

        // TODO(ngates): construct this from the SerdeContext.
        let dtype_ctx =
            DTypeSerdeContext::new(COMPOSITE_EXTENSIONS.iter().map(|e| e.id()).collect());
        let dtype = DType::read_flatbuffer(
            &dtype_ctx,
            &schema
                .dtype()
                .ok_or_else(|| vortex_err!(InvalidSerde: "Schema missing DType"))?,
        )
        .map_err(|e| vortex_err!(InvalidSerde: "Failed to parse DType: {}", e))?;

        Ok(Some(StreamArrayChunkReader {
            read: &mut self.read,
            ctx: &self.ctx,
            dtype,
            fb_buffer: Vec::new(),
            buffers: Vec::new(),
        }))
    }
}

#[allow(dead_code)]
pub struct StreamArrayChunkReader<'a, R: Read> {
    read: &'a mut R,
    ctx: &'a SerdeContext,
    dtype: DType,
    fb_buffer: Vec<u8>,
    buffers: Vec<Buffer>,
}

impl<'a, R: Read> StreamArrayChunkReader<'a, R> {
    pub fn dtype(&self) -> &DType {
        &self.dtype
    }
}

#[gat]
impl<'a, R: Read> FallibleLendingIterator for StreamArrayChunkReader<'a, R> {
    type Error = VortexError;
    type Item<'next> = ArrayView<'next> where Self: 'next;

    fn next(&mut self) -> Result<Option<ArrayView<'_>>, Self::Error> {
        let mut fb_vec: Vec<u8> = Vec::new();
        let msg = self.read.read_message::<Message>(&mut fb_vec)?;
        if msg.is_none() {
            // End of the stream
            return Ok(None);
        }
        let msg = msg.unwrap();

        let chunk = msg
            .header_as_chunk()
            .ok_or_else(|| vortex_err!(InvalidSerde: "Expected IPC Chunk message"))
            .unwrap();

        let col_offsets = chunk
            .column_offsets()
            .ok_or_else(
                || vortex_err!(InvalidSerde: "Expected column offsets in IPC Chunk message"),
            )
            .unwrap();
        assert_eq!(col_offsets.len(), 1);

        // TODO(ngates): read each column
        read_into(self.read, &mut self.fb_buffer).unwrap();
        let col_msg = root::<Message>(&self.fb_buffer)
            .unwrap()
            .header_as_chunk_column()
            .ok_or_else(|| vortex_err!(InvalidSerde: "Expected IPC Chunk Column message"))
            .unwrap();

        let col_array = col_msg
            .array()
            .ok_or_else(|| vortex_err!(InvalidSerde: "Chunk column missing Array"))
            .unwrap();

        // Read all the column's buffers
        self.buffers.clear();
        let mut offset = 0;
        for buffer in col_msg.buffers().unwrap_or_default().iter() {
            let to_kill = buffer.offset() - offset;
            io::copy(&mut self.read.take(to_kill), &mut io::sink()).unwrap();

            let mut bytes = vec![0u8; buffer.length() as usize];
            self.read.read_exact(&mut bytes).unwrap();
            self.buffers.push(Buffer::from_vec(bytes));

            offset = buffer.offset() + buffer.length();
        }

        // Consume any remaining padding after the final buffer.
        let to_kill = col_msg.buffer_size() - offset;
        io::copy(&mut self.read.take(to_kill), &mut io::sink()).unwrap();

        let view = ArrayView::try_new(self.ctx, &self.dtype, col_array, &self.buffers)?;

        // Validate the array once here so we can ignore metadata parsing errors from now on.
        // TODO(ngates): should we convert to heap-allocated array if this is missing?
        view.vtable().validate(&view)?;

        Ok(Some(view))
    }
}

/// FIXME(ngates): this exists to detach the lifetimes of the object as read by read_flatbuffer.
///  We should be able to fix that.
pub fn read_into<R: Read>(read: &mut R, buffer: &mut Vec<u8>) -> VortexResult<()> {
    buffer.clear();

    let mut buffer_len: [u8; 4] = [0; 4];
    // FIXME(ngates): return optional for EOF?
    read.read_exact(&mut buffer_len)?;

    let buffer_len = u32::from_le_bytes(buffer_len) as usize;
    read.take(buffer_len as u64).read_to_end(buffer)?;

    Ok(())
}