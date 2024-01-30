use std::any::Any;
use std::sync::{Arc, RwLock};

use crate::array::formatter::{ArrayDisplay, ArrayFormatter};
use arrow::array::StructArray as ArrowStructArray;
use arrow::array::{Array as ArrowArray, ArrayRef as ArrowArrayRef};
use arrow::datatypes::Fields;
use itertools::Itertools;

use crate::array::stats::{Stats, StatsSet};
use crate::arrow::aligned_iter::AlignedArrowArrayIterator;
use crate::error::EncResult;
use crate::scalar::{Scalar, StructScalar};
use crate::types::{DType, FieldNames};

use super::{Array, ArrayRef, ArrowIterator, Encoding, EncodingId, EncodingRef};

#[derive(Debug, Clone)]
pub struct StructArray {
    fields: Vec<ArrayRef>,
    dtype: DType,
    stats: Arc<RwLock<StatsSet>>,
}

impl StructArray {
    pub fn new(names: Vec<&str>, fields: Vec<ArrayRef>) -> Self {
        assert!(
            fields.iter().map(|v| v.len()).all_equal(),
            "Fields didn't have the same length"
        );
        let field_names: FieldNames = names.iter().map(|s| Arc::new((*s).to_owned())).collect();
        let dtype = DType::Struct(
            field_names,
            fields.iter().map(|a| a.dtype().clone()).collect(),
        );
        Self {
            fields,
            dtype,
            stats: Arc::new(RwLock::new(StatsSet::new())),
        }
    }

    #[inline]
    pub fn fields(&self) -> &[ArrayRef] {
        &self.fields
    }
}

impl Array for StructArray {
    #[inline]
    fn as_any(&self) -> &dyn Any {
        self
    }

    #[inline]
    fn boxed(self) -> ArrayRef {
        Box::new(self)
    }

    #[inline]
    fn into_any(self: Box<Self>) -> Box<dyn Any> {
        self
    }

    fn len(&self) -> usize {
        self.fields.first().map_or(0, |a| a.len())
    }

    #[inline]
    fn is_empty(&self) -> bool {
        self.len() == 0
    }

    #[inline]
    fn dtype(&self) -> &DType {
        &self.dtype
    }

    #[inline]
    fn stats(&self) -> Stats {
        Stats::new(&self.stats, self)
    }

    fn scalar_at(&self, index: usize) -> EncResult<Box<dyn Scalar>> {
        Ok(Box::new(StructScalar::new(
            self.dtype.clone(),
            self.fields
                .iter()
                .map(|field| field.scalar_at(index))
                .try_collect()?,
        )))
    }

    fn iter_arrow(&self) -> Box<ArrowIterator> {
        let fields: Fields = self.dtype().into();
        Box::new(
            AlignedArrowArrayIterator::new(
                self.fields
                    .iter()
                    .map(|f| f.iter_arrow())
                    .collect::<Vec<_>>(),
            )
            .map(move |items| {
                Arc::new(ArrowStructArray::new(
                    fields.clone(),
                    items.into_iter().map(ArrowArrayRef::from).collect(),
                    None,
                )) as Arc<dyn ArrowArray>
            }),
        )
    }

    fn slice(&self, start: usize, stop: usize) -> EncResult<ArrayRef> {
        self.check_slice_bounds(start, stop)?;

        let fields = self
            .fields
            .iter()
            .map(|field| field.slice(start, stop))
            .try_collect()?;
        Ok(Box::new(Self {
            fields,
            dtype: self.dtype.clone(),
            stats: Arc::new(RwLock::new(StatsSet::new())),
        }))
    }

    #[inline]
    fn encoding(&self) -> EncodingRef {
        &StructEncoding
    }

    fn nbytes(&self) -> usize {
        self.fields.iter().map(|arr| arr.nbytes()).sum()
    }
}

impl<'arr> AsRef<(dyn Array + 'arr)> for StructArray {
    fn as_ref(&self) -> &(dyn Array + 'arr) {
        self
    }
}

#[derive(Debug)]
pub struct StructEncoding;

pub const STRUCT_ENCODING: EncodingId = EncodingId("enc.struct");

impl Encoding for StructEncoding {
    fn id(&self) -> &EncodingId {
        &STRUCT_ENCODING
    }
}

impl ArrayDisplay for StructArray {
    fn fmt(&self, f: &mut ArrayFormatter) -> std::fmt::Result {
        let DType::Struct(n, _) = self.dtype() else {
            unreachable!()
        };
        for (name, field) in n.iter().zip(self.fields()) {
            f.writeln(format!("{}:", &**name))?;
            f.indent(|indented| indented.array(field.as_ref()))?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod test {
    use std::sync::Arc;

    use arrow::array::types::UInt64Type;
    use arrow::array::PrimitiveArray as ArrowPrimitiveArray;
    use arrow::array::StructArray as ArrowStructArray;
    use arrow::array::{Array as ArrowArray, GenericStringArray as ArrowStringArray};

    use crate::array::struct_::StructArray;
    use crate::array::Array;

    #[test]
    pub fn iter() {
        let arrow_aas = ArrowPrimitiveArray::<UInt64Type>::from(vec![1, 2, 3]);
        let arrow_bbs = ArrowStringArray::<i32>::from(vec!["a", "b", "c"]);

        let array = StructArray::new(
            vec!["a", "b"],
            vec![(&arrow_aas).into(), (&arrow_bbs).into()],
        );
        let arrow_struct = ArrowStructArray::new(
            array.dtype().into(),
            vec![Arc::new(arrow_aas), Arc::new(arrow_bbs)],
            None,
        );

        assert_eq!(
            array
                .iter_arrow()
                .next()
                .unwrap()
                .as_any()
                .downcast_ref::<ArrowStructArray>()
                .unwrap(),
            &arrow_struct
        );
    }
}
