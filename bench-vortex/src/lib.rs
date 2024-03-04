use itertools::Itertools;

use vortex::array::Encoding;
use vortex_alp::ALPEncoding;
use vortex_dict::DictEncoding;
use vortex_fastlanes::{BitPackedEncoding, FoREncoding};
use vortex_ree::REEEncoding;
use vortex_roaring::{RoaringBoolEncoding, RoaringIntEncoding};
use vortex_zigzag::ZigZagEncoding;

pub fn enumerate_arrays() {
    let encodings: Vec<&dyn Encoding> = vec![
        &ALPEncoding,
        &DictEncoding,
        &BitPackedEncoding,
        &FoREncoding,
        //&FFoREncoding,
        &REEEncoding,
        &RoaringBoolEncoding,
        &RoaringIntEncoding,
        &ZigZagEncoding,
    ];
    println!("{}", encodings.iter().map(|e| e.id()).format(", "));
}

#[cfg(test)]
mod test {
    use std::fs::create_dir_all;
    use std::fs::File;
    use std::path::Path;

    use arrow_array::RecordBatchReader;
    use log::LevelFilter;
    use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;
    use simplelog::{ColorChoice, Config, TermLogger, TerminalMode};

    use vortex::array::chunked::ChunkedArray;
    use vortex::array::{Array, ArrayRef};
    use vortex::compress::CompressCtx;
    use vortex::dtype::DType;
    use vortex::error::{VortexError, VortexResult};

    use crate::enumerate_arrays;

    pub fn download_taxi_data() -> &'static Path {
        let download_path = Path::new("data/yellow-tripdata-2023-11.parquet");
        if download_path.exists() {
            return download_path;
        }

        create_dir_all(download_path.parent().unwrap()).unwrap();
        let mut download_file = File::create(download_path).unwrap();
        reqwest::blocking::get(
            "https://d37ci6vzurychx.cloudfront.net/trip-data/yellow_tripdata_2023-11.parquet",
        )
        .unwrap()
        .copy_to(&mut download_file)
        .unwrap();

        download_path
    }

    fn setup_logger() {
        TermLogger::init(
            LevelFilter::Info,
            Config::default(),
            TerminalMode::Mixed,
            ColorChoice::Auto,
        )
        .unwrap();
    }

    #[test]
    fn compression_ratio() {
        enumerate_arrays();
        setup_logger();

        let file = File::open(download_taxi_data()).unwrap();
        let builder = ParquetRecordBatchReaderBuilder::try_new(file).unwrap();
        let reader = builder.with_batch_size(128_000).build().unwrap();

        let schema = reader.schema();
        let dtype: DType = schema.try_into().unwrap();
        let chunks = reader
            .map(|batch_result| batch_result.map_err(VortexError::from))
            .map(|batch| batch.map(|b| b.into()))
            .collect::<VortexResult<Vec<ArrayRef>>>()
            .unwrap();
        let chunked = ChunkedArray::new(chunks, dtype);
        println!(
            "{} rows in {} chunks",
            chunked.len(),
            chunked.chunks().len()
        );
        let array = chunked.boxed();
        let compressed = CompressCtx::default().compress(array.as_ref(), None);
        println!("Compressed array {compressed}");
        println!(
            "NBytes {}, Ratio {}",
            compressed.nbytes(),
            compressed.nbytes() as f32 / array.nbytes() as f32
        );
    }
}