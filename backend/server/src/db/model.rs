pub mod build;
pub mod drv;
pub mod git;

/// A variant of some type `T` that can only be used for insertion in the database.
///
/// This type is useful if some fields of a model type are filled in automatically during insertion
/// and therefore require dummy values when constructing the type directly. These types can provide
/// a constructor function that returns `ForInsert<Self` which allows reusing the type definition
/// of `T` but does not allow the caller to access the inner `T`. The database routines however can
/// access the inner type and only use the known good values, letting the database fill in the rest
/// of the values automatically.
pub struct ForInsert<T>(pub(super) T);

impl<T: Clone> Clone for ForInsert<T> {
    fn clone(&self) -> Self {
        Self(self.0.clone())
    }
}
