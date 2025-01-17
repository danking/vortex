use ::serde::{Deserialize, Serialize};
use vortex_dtype::{match_each_integer_ptype, DType};
use vortex_error::{vortex_bail, VortexResult};
use vortex_scalar::Scalar;

use crate::array::constant::ConstantArray;
use crate::compute::unary::scalar_at;
use crate::compute::{search_sorted, SearchSortedSide};
use crate::stats::{ArrayStatisticsCompute, StatsSet};
use crate::validity::{ArrayValidity, LogicalValidity};
use crate::visitor::{AcceptArrayVisitor, ArrayVisitor};
use crate::{impl_encoding, Array, ArrayDType, ArrayDef, ArrayTrait, IntoArray, IntoArrayVariant};

mod compute;
mod flatten;
mod variants;

impl_encoding!("vortex.sparse", 9u16, Sparse);

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SparseMetadata {
    indices_dtype: DType,
    // Offset value for patch indices as a result of slicing
    indices_offset: usize,
    indices_len: usize,
    len: usize,
    fill_value: Scalar,
}

impl SparseArray {
    pub fn try_new(
        indices: Array,
        values: Array,
        len: usize,
        fill_value: Scalar,
    ) -> VortexResult<Self> {
        Self::try_new_with_offset(indices, values, len, 0, fill_value)
    }

    pub(crate) fn try_new_with_offset(
        indices: Array,
        values: Array,
        len: usize,
        indices_offset: usize,
        fill_value: Scalar,
    ) -> VortexResult<Self> {
        if !matches!(indices.dtype(), &DType::IDX) {
            vortex_bail!("Cannot use {} as indices", indices.dtype());
        }
        if values.dtype() != fill_value.dtype() {
            vortex_bail!(
                "Mismatched fill value dtype {} and values dtype {}",
                fill_value.dtype(),
                values.dtype(),
            );
        }
        if indices.len() != values.len() {
            vortex_bail!(
                "Mismatched indices {} and values {} length",
                indices.len(),
                values.len()
            );
        }

        if !indices.is_empty() {
            let last_index = usize::try_from(&scalar_at(&indices, indices.len() - 1)?)?;

            if last_index - indices_offset >= len {
                vortex_bail!("Array length was set to {len} but the last index is {last_index}");
            }
        }

        Self::try_from_parts(
            values.dtype().clone(),
            len,
            SparseMetadata {
                indices_dtype: indices.dtype().clone(),
                indices_offset,
                indices_len: indices.len(),
                len,
                fill_value,
            },
            [indices, values].into(),
            StatsSet::new(),
        )
    }
}

impl SparseArray {
    #[inline]
    pub fn indices_offset(&self) -> usize {
        self.metadata().indices_offset
    }

    #[inline]
    pub fn values(&self) -> Array {
        self.array()
            .child(1, self.dtype(), self.metadata().indices_len)
            .expect("missing child array")
    }

    #[inline]
    pub fn indices(&self) -> Array {
        self.array()
            .child(
                0,
                &self.metadata().indices_dtype,
                self.metadata().indices_len,
            )
            .expect("missing indices array")
    }

    #[inline]
    pub fn fill_value(&self) -> &Scalar {
        &self.metadata().fill_value
    }

    /// Returns the position of a given index in the indices array if it exists.
    pub fn find_index(&self, index: usize) -> VortexResult<Option<usize>> {
        search_sorted(
            &self.indices(),
            self.indices_offset() + index,
            SearchSortedSide::Left,
        )
        .map(|r| r.to_found())
    }

    /// Return indices as a vector of usize with the indices_offset applied.
    pub fn resolved_indices(&self) -> Vec<usize> {
        let flat_indices = self.indices().into_primitive().unwrap_or_else(|err| {
            panic!("Failed to convert indices to primitive array: {}", err);
        });
        match_each_integer_ptype!(flat_indices.ptype(), |$P| {
            flat_indices
                .maybe_null_slice::<$P>()
                .iter()
                .map(|v| (*v as usize) - self.indices_offset())
                .collect::<Vec<_>>()
        })
    }

    pub fn min_index(&self) -> usize {
        let min_index: usize = scalar_at(&self.indices(), 0)
            .and_then(|s| s.as_ref().try_into())
            .unwrap_or_else(|err| {
                panic!("Failed to get min_index: {}", err);
            });
        min_index - self.indices_offset()
    }
}

impl ArrayTrait for SparseArray {}

impl AcceptArrayVisitor for SparseArray {
    fn accept(&self, visitor: &mut dyn ArrayVisitor) -> VortexResult<()> {
        visitor.visit_child("indices", &self.indices())?;
        visitor.visit_child("values", &self.values())
    }
}

impl ArrayStatisticsCompute for SparseArray {}

impl ArrayValidity for SparseArray {
    fn is_valid(&self, index: usize) -> bool {
        match self.find_index(index).unwrap_or_else(|err| {
            panic!(
                "Error while finding index {} in sparse array: {}",
                index, err
            );
        }) {
            None => !self.fill_value().is_null(),
            Some(idx) => self.values().with_dyn(|a| a.is_valid(idx)),
        }
    }

    fn logical_validity(&self) -> LogicalValidity {
        let validity = if self.fill_value().is_null() {
            // If we have a null fill value, then the result is a Sparse array with a fill_value
            // of true, and patch values of false.
            Self::try_new_with_offset(
                self.indices(),
                ConstantArray::new(true, self.indices().len()).into_array(),
                self.len(),
                self.indices_offset(),
                false.into(),
            )
        } else {
            // If the fill_value is non-null, then the validity is based on the validity of the
            // existing values.
            Self::try_new_with_offset(
                self.indices(),
                self.values()
                    .with_dyn(|a| a.logical_validity().into_array()),
                self.len(),
                self.indices_offset(),
                false.into(),
            )
        }
        .unwrap_or_else(|err| {
            panic!(
                "Error determining logical validity for sparse array: {}",
                err
            );
        });

        LogicalValidity::Array(validity.into_array())
    }
}

#[cfg(test)]
mod test {
    use itertools::Itertools;
    use vortex_dtype::Nullability::{self, Nullable};
    use vortex_dtype::{DType, PType};
    use vortex_error::VortexError;
    use vortex_scalar::Scalar;

    use crate::accessor::ArrayAccessor;
    use crate::array::sparse::SparseArray;
    use crate::compute::slice;
    use crate::compute::unary::{scalar_at, try_cast};
    use crate::{Array, IntoArray, IntoArrayVariant};

    fn nullable_fill() -> Scalar {
        Scalar::null(DType::Primitive(PType::I32, Nullable))
    }

    #[allow(dead_code)]
    fn non_nullable_fill() -> Scalar {
        Scalar::from(42i32)
    }

    fn sparse_array(fill_value: Scalar) -> Array {
        // merged array: [null, null, 100, null, null, 200, null, null, 300, null]
        let mut values = vec![100i32, 200, 300].into_array();
        values = try_cast(&values, fill_value.dtype()).unwrap();

        SparseArray::try_new(vec![2u64, 5, 8].into_array(), values, 10, fill_value)
            .unwrap()
            .into_array()
    }

    fn assert_sparse_array(sparse: &Array, values: &[Option<i32>]) {
        let sparse_arrow = ArrayAccessor::<i32>::with_iterator(
            &sparse.clone().into_primitive().unwrap(),
            |iter| iter.map(|v| v.cloned()).collect_vec(),
        )
        .unwrap();
        assert_eq!(&sparse_arrow, values);
    }

    #[test]
    pub fn iter() {
        assert_sparse_array(
            &sparse_array(nullable_fill()),
            &[
                None,
                None,
                Some(100),
                None,
                None,
                Some(200),
                None,
                None,
                Some(300),
                None,
            ],
        );
    }

    #[test]
    pub fn iter_sliced() {
        let p_fill_val = Some(non_nullable_fill().as_ref().try_into().unwrap());
        assert_sparse_array(
            &slice(&sparse_array(non_nullable_fill()), 2, 7).unwrap(),
            &[Some(100), p_fill_val, p_fill_val, Some(200), p_fill_val],
        );
    }

    #[test]
    pub fn iter_sliced_nullable() {
        assert_sparse_array(
            &slice(&sparse_array(nullable_fill()), 2, 7).unwrap(),
            &[Some(100), None, None, Some(200), None],
        );
    }

    #[test]
    pub fn iter_sliced_twice() {
        let sliced_once = slice(&sparse_array(nullable_fill()), 1, 8).unwrap();
        assert_sparse_array(
            &sliced_once,
            &[None, Some(100), None, None, Some(200), None, None],
        );
        assert_sparse_array(
            &slice(&sliced_once, 1, 6).unwrap(),
            &[Some(100), None, None, Some(200), None],
        );
    }

    #[test]
    pub fn test_find_index() {
        let sparse = SparseArray::try_from(sparse_array(nullable_fill())).unwrap();
        assert_eq!(sparse.find_index(0).unwrap(), None);
        assert_eq!(sparse.find_index(2).unwrap(), Some(0));
        assert_eq!(sparse.find_index(5).unwrap(), Some(1));
    }

    #[test]
    pub fn test_scalar_at() {
        assert_eq!(
            usize::try_from(&scalar_at(&sparse_array(nullable_fill()), 2).unwrap()).unwrap(),
            100
        );
        let error = scalar_at(&sparse_array(nullable_fill()), 10).err().unwrap();
        let VortexError::OutOfBounds(i, start, stop, _) = error else {
            unreachable!()
        };
        assert_eq!(i, 10);
        assert_eq!(start, 0);
        assert_eq!(stop, 10);
    }

    #[test]
    pub fn scalar_at_sliced() {
        let sliced = slice(&sparse_array(nullable_fill()), 2, 7).unwrap();
        assert_eq!(
            usize::try_from(&scalar_at(&sliced, 0).unwrap()).unwrap(),
            100
        );
        let error = scalar_at(&sliced, 5).err().unwrap();
        let VortexError::OutOfBounds(i, start, stop, _) = error else {
            unreachable!()
        };
        assert_eq!(i, 5);
        assert_eq!(start, 0);
        assert_eq!(stop, 5);
    }

    #[test]
    pub fn scalar_at_sliced_twice() {
        let sliced_once = slice(&sparse_array(nullable_fill()), 1, 8).unwrap();
        assert_eq!(
            usize::try_from(&scalar_at(&sliced_once, 1).unwrap()).unwrap(),
            100
        );
        let error = scalar_at(&sliced_once, 7).err().unwrap();
        let VortexError::OutOfBounds(i, start, stop, _) = error else {
            unreachable!()
        };
        assert_eq!(i, 7);
        assert_eq!(start, 0);
        assert_eq!(stop, 7);

        let sliced_twice = slice(&sliced_once, 1, 6).unwrap();
        assert_eq!(
            usize::try_from(&scalar_at(&sliced_twice, 3).unwrap()).unwrap(),
            200
        );
        let error2 = scalar_at(&sliced_twice, 5).err().unwrap();
        let VortexError::OutOfBounds(i, start, stop, _) = error2 else {
            unreachable!()
        };
        assert_eq!(i, 5);
        assert_eq!(start, 0);
        assert_eq!(stop, 5);
    }

    #[test]
    pub fn sparse_logical_validity() {
        let array = sparse_array(nullable_fill());
        let validity = array
            .with_dyn(|a| a.logical_validity())
            .into_array()
            .into_bool()
            .unwrap();
        assert_eq!(
            validity.boolean_buffer().iter().collect_vec(),
            [false, false, true, false, false, true, false, false, true, false]
        );
    }

    #[test]
    #[should_panic]
    fn test_invalid_length() {
        let values = vec![15_u32, 135, 13531, 42].into_array();
        let indices = vec![10_u64, 11, 50, 100].into_array();

        SparseArray::try_new(
            indices.clone(),
            values,
            100,
            Scalar::primitive(0_u32, Nullability::NonNullable),
        )
        .unwrap();
    }

    #[test]
    fn test_valid_length() {
        let values = vec![15_u32, 135, 13531, 42].into_array();
        let indices = vec![10_u64, 11, 50, 100].into_array();

        SparseArray::try_new(
            indices.clone(),
            values,
            101,
            Scalar::primitive(0_u32, Nullability::NonNullable),
        )
        .unwrap();
    }
}
