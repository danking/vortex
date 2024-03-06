use crate::array::{Array, ArrayRef};
use crate::error::{VortexError, VortexResult};

pub trait TakeFn {
    fn take(&self, indices: &dyn Array) -> VortexResult<ArrayRef>;
}

pub fn take(array: &dyn Array, indices: &dyn Array) -> VortexResult<ArrayRef> {
    array
        .take()
        .map(|t| t.take(indices))
        .unwrap_or_else(|| Err(VortexError::NotImplemented("take", array.encoding().id())))
}