//! Capacity-enforced domain collections.

use core::{fmt, marker::PhantomData, ops::Deref, slice};

use serde::{de, Deserialize, Deserializer};

/// Error returned when constructing or extending a bounded collection.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CapacityError {
    capacity: usize,
    attempted_len: usize,
}

impl CapacityError {
    /// Constructs capacity failure details.
    pub const fn new(capacity: usize, attempted_len: usize) -> Self {
        Self {
            capacity,
            attempted_len,
        }
    }

    /// Returns the declared maximum element count.
    pub const fn capacity(self) -> usize {
        self.capacity
    }

    /// Returns the element count that was rejected.
    pub const fn attempted_len(self) -> usize {
        self.attempted_len
    }
}

impl fmt::Display for CapacityError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "collection would contain {} elements; reduce it to at most {}",
            self.attempted_len, self.capacity
        )
    }
}

impl std::error::Error for CapacityError {}

/// A vector whose maximum length cannot be bypassed through its public API.
///
/// The inner vector is private. Any future deserializer must call
/// [`Self::try_from_vec`] rather than constructing the value directly.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BoundedVec<T, const CAPACITY: usize>(Vec<T>);

impl<T, const CAPACITY: usize> BoundedVec<T, CAPACITY> {
    /// Constructs an empty bounded vector.
    pub const fn new() -> Self {
        Self(Vec::new())
    }

    /// Validates an existing vector without truncating data.
    pub fn try_from_vec(values: Vec<T>) -> Result<Self, CapacityError> {
        if values.len() > CAPACITY {
            return Err(CapacityError::new(CAPACITY, values.len()));
        }
        Ok(Self(values))
    }

    /// Constructs a bounded vector by truncating `values` to `CAPACITY`
    /// elements. The bounded type's `try_from_vec` constructor fails when
    /// the input is over capacity, which is the right semantics for a
    /// deserializer (we must not silently drop a parsed element) but the
    /// wrong semantics for a *known* hardcoded literal in the source tree
    /// (a future contributor who widens the literal should see a silent
    /// truncation in the test/build output, not a process-kill). The
    /// infallible constructor follows the same pattern as
    /// `BoundedText::from_nonempty_clamped`: a defensive fallback for the
    /// hardcoded case, not a license to overflow the bound in production.
    pub fn from_vec_truncated(mut values: Vec<T>) -> Self {
        if values.len() > CAPACITY {
            values.truncate(CAPACITY);
        }
        Self(values)
    }

    /// Appends one value or returns it unchanged when capacity is exhausted.
    pub fn try_push(&mut self, value: T) -> Result<(), BoundedPushError<T>> {
        if self.0.len() == CAPACITY {
            return Err(BoundedPushError {
                value,
                capacity: CAPACITY,
            });
        }
        self.0.push(value);
        Ok(())
    }

    /// Extends atomically with respect to capacity validation.
    ///
    /// No values are appended when the resulting length would exceed capacity.
    pub fn try_extend(&mut self, values: Vec<T>) -> Result<(), BoundedExtendError<T>> {
        let attempted_len = self.0.len().saturating_add(values.len());
        if attempted_len > CAPACITY {
            return Err(BoundedExtendError {
                values,
                error: CapacityError::new(CAPACITY, attempted_len),
            });
        }
        self.0.extend(values);
        Ok(())
    }

    /// Returns the current number of elements.
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// Returns whether the collection contains no elements.
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Returns the declared maximum element count.
    pub const fn capacity(&self) -> usize {
        CAPACITY
    }

    /// Borrows all validated elements.
    pub fn as_slice(&self) -> &[T] {
        &self.0
    }

    /// Iterates over validated elements.
    pub fn iter(&self) -> slice::Iter<'_, T> {
        self.0.iter()
    }

    /// Mutably iterates without changing collection length.
    pub fn iter_mut(&mut self) -> slice::IterMut<'_, T> {
        self.0.iter_mut()
    }

    /// Consumes the wrapper and returns its elements.
    pub fn into_vec(self) -> Vec<T> {
        self.0
    }
}

impl<T, const CAPACITY: usize> Default for BoundedVec<T, CAPACITY> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T, const CAPACITY: usize> Deref for BoundedVec<T, CAPACITY> {
    type Target = [T];

    fn deref(&self) -> &Self::Target {
        self.as_slice()
    }
}

impl<'a, T, const CAPACITY: usize> IntoIterator for &'a BoundedVec<T, CAPACITY> {
    type Item = &'a T;
    type IntoIter = slice::Iter<'a, T>;

    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

impl<'a, T, const CAPACITY: usize> IntoIterator for &'a mut BoundedVec<T, CAPACITY> {
    type Item = &'a mut T;
    type IntoIter = slice::IterMut<'a, T>;

    fn into_iter(self) -> Self::IntoIter {
        self.iter_mut()
    }
}

impl<'de, T, const CAPACITY: usize> Deserialize<'de> for BoundedVec<T, CAPACITY>
where
    T: Deserialize<'de>,
{
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_seq(BoundedVecVisitor::<T, CAPACITY>(PhantomData))
    }
}

impl<T, const CAPACITY: usize> serde::Serialize for BoundedVec<T, CAPACITY>
where
    T: serde::Serialize,
{
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.collect_seq(self.iter())
    }
}

struct BoundedVecVisitor<T, const CAPACITY: usize>(PhantomData<T>);

impl<'de, T, const CAPACITY: usize> de::Visitor<'de> for BoundedVecVisitor<T, CAPACITY>
where
    T: Deserialize<'de>,
{
    type Value = BoundedVec<T, CAPACITY>;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "a sequence containing at most {CAPACITY} elements"
        )
    }

    fn visit_seq<A>(self, mut sequence: A) -> Result<Self::Value, A::Error>
    where
        A: de::SeqAccess<'de>,
    {
        let hinted = match sequence.size_hint() {
            Some(value) => value.min(CAPACITY),
            None => 0,
        };
        let mut values = Vec::with_capacity(hinted);
        while let Some(value) = sequence.next_element()? {
            if values.len() == CAPACITY {
                return Err(de::Error::custom(CapacityError::new(
                    CAPACITY,
                    CAPACITY + 1,
                )));
            }
            values.push(value);
        }
        Ok(BoundedVec(values))
    }
}

/// A rejected value returned to its caller instead of being silently dropped.
#[derive(Debug, Eq, PartialEq)]
pub struct BoundedPushError<T> {
    value: T,
    capacity: usize,
}

impl<T> BoundedPushError<T> {
    /// Recovers ownership of the rejected value.
    pub fn into_value(self) -> T {
        self.value
    }

    /// Returns the collection capacity that was reached.
    pub const fn capacity(&self) -> usize {
        self.capacity
    }
}

impl<T> fmt::Display for BoundedPushError<T> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "collection capacity {} is exhausted",
            self.capacity
        )
    }
}

impl<T: fmt::Debug> std::error::Error for BoundedPushError<T> {}

/// An atomically rejected extension that retains all input values.
#[derive(Debug, Eq, PartialEq)]
pub struct BoundedExtendError<T> {
    values: Vec<T>,
    error: CapacityError,
}

impl<T> BoundedExtendError<T> {
    /// Returns capacity failure details.
    pub const fn capacity_error(&self) -> CapacityError {
        self.error
    }
}

impl<T> fmt::Display for BoundedExtendError<T> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.error.fmt(formatter)
    }
}

impl<T: fmt::Debug> std::error::Error for BoundedExtendError<T> {}

#[cfg(test)]
mod tests {
    use super::BoundedVec;

    #[test]
    fn extension_is_all_or_nothing() {
        let mut values =
            BoundedVec::<u8, 2>::try_from_vec(vec![1]).map_err(|error| error.to_string());
        let result = values
            .as_mut()
            .map(|bounded| bounded.try_extend(vec![2, 3]));
        assert!(matches!(result, Ok(Err(_))));
        assert_eq!(values.map(BoundedVec::into_vec), Ok(vec![1]));
    }

    #[test]
    fn constructor_rejects_oversized_input() {
        let result = BoundedVec::<u8, 1>::try_from_vec(vec![1, 2]);
        assert_eq!(
            result.map(|values| values.len()),
            Err(super::CapacityError::new(1, 2))
        );
    }

    #[test]
    fn deserialization_cannot_bypass_capacity() {
        let result = serde_json::from_str::<BoundedVec<u8, 1>>("[1,2]");
        assert!(result.is_err());
    }

    /// `from_vec_truncated` is the infallible constructor used in
    /// production paths where the caller knows the bounded capacity will
    /// accommodate the input (or has already decided truncation is
    /// acceptable). The previous `try_from_vec` + `unwrap_or_else(|_| abort)`
    /// pattern was a process-kill fallback for the (rare but real) over-long
    /// path; this test pins the new fall-back behaviour across the
    /// under/over/empty input cases that previously lived as three
    /// redundant single-assertion tests.
    #[test]
    fn from_vec_truncated_keeps_under_truncates_over_and_handles_empty() {
        // Under capacity: every element survives in order.
        let under = BoundedVec::<u8, 4>::from_vec_truncated(vec![1, 2, 3]);
        assert_eq!(under.len(), 3);
        assert_eq!(under.as_slice(), &[1, 2, 3]);
        // Over capacity: keeps the leading elements up to CAPACITY.
        let over = BoundedVec::<u8, 2>::from_vec_truncated(vec![1, 2, 3, 4, 5]);
        assert_eq!(over.len(), 2, "truncated to capacity");
        assert_eq!(over.as_slice(), &[1, 2], "first elements kept");
        // Empty input: zero-length bounded vector, no panic.
        let empty = BoundedVec::<u8, 4>::from_vec_truncated(Vec::new());
        assert!(empty.is_empty());
    }
}
