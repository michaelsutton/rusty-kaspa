use crate::{MAX_SCRIPT_ELEMENT_SIZE_POST_TOCCATA, TxScriptError};
use core::fmt::Debug;
use core::iter;
use kaspa_hashes::Hash;
use kaspa_txscript_errors::SerializationError;
use smallvec::SmallVec;
use std::cmp::Ordering;
use std::num::TryFromIntError;
use std::ops::{Deref, Index};

#[derive(PartialEq, Eq, Debug, Default, PartialOrd, Ord)]
pub(crate) struct SizedEncodeInt<const LEN: usize>(pub(crate) i64);

impl<const LEN: usize> From<i64> for SizedEncodeInt<LEN> {
    fn from(value: i64) -> Self {
        SizedEncodeInt(value)
    }
}

impl<const LEN: usize> From<i32> for SizedEncodeInt<LEN> {
    fn from(value: i32) -> Self {
        SizedEncodeInt(value as i64)
    }
}

impl<const LEN: usize> TryFrom<SizedEncodeInt<LEN>> for i32 {
    type Error = TryFromIntError;

    fn try_from(value: SizedEncodeInt<LEN>) -> Result<Self, Self::Error> {
        value.0.try_into()
    }
}

impl<const LEN: usize> PartialEq<i64> for SizedEncodeInt<LEN> {
    fn eq(&self, other: &i64) -> bool {
        self.0 == *other
    }
}

impl<const LEN: usize> PartialOrd<i64> for SizedEncodeInt<LEN> {
    fn partial_cmp(&self, other: &i64) -> Option<Ordering> {
        self.0.partial_cmp(other)
    }
}

impl<const LEN: usize> Deref for SizedEncodeInt<LEN> {
    type Target = i64;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl<const LEN: usize> From<SizedEncodeInt<LEN>> for i64 {
    fn from(value: SizedEncodeInt<LEN>) -> Self {
        value.0
    }
}

#[inline]
fn total_bytes(items: &[StackEntry]) -> usize {
    items.iter().map(|item| item.len()).sum()
}

pub type StackEntry = SmallVec<[u8; 8]>;

#[derive(Clone, Debug)]
pub(crate) struct Stack {
    inner: Vec<StackEntry>,
    covenants_enabled: bool,
    pushed_bytes: u64,
}

#[cfg(test)]
impl PartialEq for Stack {
    fn eq(&self, other: &Self) -> bool {
        self.inner == other.inner && self.covenants_enabled == other.covenants_enabled
    }
}

#[cfg(test)]
impl Eq for Stack {}

impl Deref for Stack {
    type Target = Vec<StackEntry>;

    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

impl Index<usize> for Stack {
    type Output = StackEntry;

    fn index(&self, index: usize) -> &Self::Output {
        &self.inner[index]
    }
}

#[cfg(test)]
impl From<Vec<StackEntry>> for Stack {
    fn from(inner: Vec<StackEntry>) -> Self {
        // TODO(covpp-mainnet): should have fork logic
        Self { inner, covenants_enabled: true, pushed_bytes: 0 }
    }
}

#[cfg(test)]
impl From<Vec<Vec<u8>>> for Stack {
    fn from(inner: Vec<Vec<u8>>) -> Self {
        Self::from(inner.into_iter().map(SmallVec::from_vec).collect::<Vec<_>>())
    }
}

impl From<Stack> for Vec<StackEntry> {
    fn from(stack: Stack) -> Self {
        stack.inner
    }
}

pub(crate) trait OpcodeData<T> {
    fn deserialize(&self, enforce_minimal: bool) -> Result<T, TxScriptError>;
    fn serialize(from: &T) -> Result<Self, SerializationError>
    where
        Self: Sized;
}

fn check_minimal_data_encoding(v: &[u8]) -> Result<(), TxScriptError> {
    if v.is_empty() {
        return Ok(());
    }

    // Check that the number is encoded with the minimum possible
    // number of bytes.
    //
    // If the most-significant-byte - excluding the sign bit - is zero
    // then we're not minimal. Note how this test also rejects the
    // negative-zero encoding, [0x80].
    if v[v.len() - 1] & 0x7f == 0 {
        // One exception: if there's more than one byte and the most
        // significant bit of the second-most-significant-byte is set
        // it would conflict with the sign bit. An example of this case
        // is +-255, which encode to 0xff00 and 0xff80 respectively.
        // (big-endian).
        if v.len() == 1 || v[v.len() - 2] & 0x80 == 0 {
            return Err(TxScriptError::NotMinimalData(format!("numeric value encoded as {v:x?} is not minimally encoded")));
        }
    }

    Ok(())
}

#[inline]
pub fn serialize_i64(from: i64, size: Option<usize>) -> Result<StackEntry, SerializationError> {
    let sign = from.signum();
    let mut positive = from.unsigned_abs();
    let mut last_saturated = false;
    let mut number_vec = StackEntry::with_capacity(size.unwrap_or(8));
    number_vec.extend(iter::from_fn(move || {
        if positive == 0 {
            if last_saturated {
                last_saturated = false;
                Some(0)
            } else {
                None
            }
        } else {
            let value = positive & 0xff;
            last_saturated = (value & 0x80) != 0;
            positive >>= 8;
            Some(value as u8)
        }
    }));

    if let Some(size) = size {
        if number_vec.len() > size {
            return Err(SerializationError::NumberTooLong(from, size));
        }
        number_vec.resize(size, 0);
    }

    if sign == -1 {
        match number_vec.last_mut() {
            Some(num) => *num |= 0x80,
            _ => unreachable!(),
        }
    }
    Ok(number_vec)
}

pub fn deserialize_i64(v: &[u8], enforce_minimal: bool) -> Result<i64, TxScriptError> {
    match v.len() {
        l if l > size_of::<i64>() => {
            // Even when `enforce_minimal` is false, we limit
            Err(TxScriptError::NotMinimalData(format!("numeric value encoded as {v:x?} is longer than 8 bytes")))
        }
        0 => Ok(0),
        _ => {
            if enforce_minimal {
                check_minimal_data_encoding(v)?;
            }

            let msb = v[v.len() - 1];
            let sign = 1 - 2 * ((msb >> 7) as i64);
            let first_byte = (msb & 0x7f) as i64;
            Ok(v[..v.len() - 1].iter().rev().map(|v| *v as i64).fold(first_byte, |accum, item| (accum << 8) + item) * sign)
        }
    }
}

impl OpcodeData<i64> for StackEntry {
    #[inline]
    fn deserialize(&self, enforce_minimal: bool) -> Result<i64, TxScriptError> {
        OpcodeData::<SizedEncodeInt<8>>::deserialize(self, enforce_minimal).map(i64::from)
    }

    #[inline]
    fn serialize(from: &i64) -> Result<Self, SerializationError> {
        OpcodeData::<SizedEncodeInt<8>>::serialize(&(*from).into())
    }
}

impl OpcodeData<i32> for StackEntry {
    #[inline]
    fn deserialize(&self, enforce_minimal: bool) -> Result<i32, TxScriptError> {
        if enforce_minimal {
            OpcodeData::<SizedEncodeInt<4>>::deserialize(self, true).map(|v| v.try_into().expect("number is within i32 range"))
        } else {
            OpcodeData::<SizedEncodeInt<8>>::deserialize(self, false)
                .and_then(|v| v.try_into().map_err(|e: TryFromIntError| TxScriptError::NumberTooBig(e.to_string())))
        }
    }

    #[inline]
    fn serialize(from: &i32) -> Result<Self, SerializationError> {
        OpcodeData::<SizedEncodeInt<4>>::serialize(&(*from).into())
    }
}

impl<const LEN: usize> OpcodeData<SizedEncodeInt<LEN>> for StackEntry {
    #[inline]
    fn deserialize(&self, enforce_minimal: bool) -> Result<SizedEncodeInt<LEN>, TxScriptError> {
        match self.len() > LEN {
            true => Err(TxScriptError::NumberTooBig(format!(
                "numeric value encoded as {:x?} is {} bytes which exceeds the max allowed of {}",
                self,
                self.len(),
                LEN
            ))),
            false => deserialize_i64(self, enforce_minimal).map(SizedEncodeInt::<LEN>),
        }
    }

    #[inline]
    fn serialize(from: &SizedEncodeInt<LEN>) -> Result<Self, SerializationError> {
        let bytes = serialize_i64(from.0, None)?;
        if bytes.len() > LEN {
            return Err(SerializationError::NumberTooLong(from.0, LEN));
        }
        Ok(bytes)
    }
}

impl OpcodeData<bool> for StackEntry {
    #[inline]
    fn deserialize(&self, _enforce_minimal: bool) -> Result<bool, TxScriptError> {
        if self.is_empty() {
            Ok(false)
        } else {
            // Negative 0 is also considered false
            Ok(self[self.len() - 1] & 0x7f != 0x0 || self[..self.len() - 1].iter().any(|&b| b != 0x0))
        }
    }

    #[inline]
    fn serialize(from: &bool) -> Result<Self, SerializationError> {
        Ok(match from {
            true => SmallVec::from_slice(&[1]),
            false => SmallVec::new(),
        })
    }
}

#[cfg(test)]
impl<T> OpcodeData<T> for Vec<u8>
where
    StackEntry: OpcodeData<T>,
{
    #[inline]
    fn deserialize(&self, enforce_minimal: bool) -> Result<T, TxScriptError> {
        <StackEntry as OpcodeData<T>>::deserialize(&StackEntry::from_slice(self), enforce_minimal)
    }

    #[inline]
    fn serialize(from: &T) -> Result<Self, SerializationError> {
        <StackEntry as OpcodeData<T>>::serialize(from).map(|v| v.into_vec())
    }
}

impl OpcodeData<Hash> for StackEntry {
    #[inline]
    fn deserialize(&self, _: bool) -> Result<Hash, TxScriptError> {
        Hash::try_from_slice(self).map_err(|_| TxScriptError::InvalidLengthOfBlockHash(self.len()))
    }

    #[inline]
    fn serialize(from: &Hash) -> Result<Self, SerializationError> {
        Ok(from.as_bytes().as_slice().into())
    }
}

impl Stack {
    pub(crate) fn new(inner: Vec<StackEntry>, covenants_enabled: bool) -> Self {
        Self { inner, covenants_enabled, pushed_bytes: 0 }
    }

    #[inline]
    fn add_pushed_bytes(&mut self, bytes: usize) {
        self.pushed_bytes = self.pushed_bytes.checked_add(bytes as u64).expect("stack pushed-bytes accounting should never overflow");
    }

    #[inline]
    pub fn pushed_bytes(&self) -> u64 {
        self.pushed_bytes
    }

    fn max_element_size(&self) -> usize {
        // Pre-toccata the element size limit was only enforced for OP_PUSHDATA opcodes, but
        // since Pre-Toccata is missing OP_CAT, it was impossible to create elements larger
        // than 520 bytes. Therefore it's safe to compare against usize::MAX without
        // breaking consensus.
        if self.covenants_enabled { MAX_SCRIPT_ELEMENT_SIZE_POST_TOCCATA } else { usize::MAX }
    }

    #[inline]
    pub fn insert(&mut self, index: usize, element: StackEntry) -> Result<(), TxScriptError> {
        if element.len() > self.max_element_size() {
            return Err(TxScriptError::ElementTooBig(element.len(), self.max_element_size()));
        }
        self.add_pushed_bytes(element.len());
        self.inner.insert(index, element);
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn inner(&self) -> &[StackEntry] {
        &self.inner
    }

    #[inline]
    pub fn pop_items<const SIZE: usize, T: Debug>(&mut self) -> Result<[T; SIZE], TxScriptError>
    where
        StackEntry: OpcodeData<T>,
    {
        if self.len() < SIZE {
            return Err(TxScriptError::InvalidStackOperation(SIZE, self.len()));
        }
        Ok(<[T; SIZE]>::try_from(
            self.inner
                .split_off(self.len() - SIZE)
                .iter()
                .map(|v| v.deserialize(!self.covenants_enabled))
                .collect::<Result<Vec<T>, _>>()?,
        )
        .expect("Already exact item"))
    }

    #[inline]
    pub fn pop_raw<const SIZE: usize>(&mut self) -> Result<[StackEntry; SIZE], TxScriptError> {
        if self.len() < SIZE {
            return Err(TxScriptError::InvalidStackOperation(SIZE, self.len()));
        }
        Ok(<[StackEntry; SIZE]>::try_from(self.inner.split_off(self.len() - SIZE)).expect("Already exact item"))
    }

    #[inline]
    pub fn peek_raw<const SIZE: usize>(&self) -> Result<[StackEntry; SIZE], TxScriptError> {
        if self.len() < SIZE {
            return Err(TxScriptError::InvalidStackOperation(SIZE, self.len()));
        }
        Ok(<[StackEntry; SIZE]>::try_from(self.inner[self.len() - SIZE..].to_vec()).expect("Already exact item"))
    }

    #[inline]
    pub fn push_item<T: Debug>(&mut self, item: T) -> Result<(), TxScriptError>
    where
        StackEntry: OpcodeData<T>,
    {
        let v: StackEntry = OpcodeData::serialize(&item)?;
        self.add_pushed_bytes(v.len());
        Vec::push(&mut self.inner, v);
        Ok(())
    }

    #[inline]
    pub fn push_item_unmetered<T: Debug>(&mut self, item: T) -> Result<(), TxScriptError>
    where
        StackEntry: OpcodeData<T>,
    {
        let v: StackEntry = OpcodeData::serialize(&item)?;
        Vec::push(&mut self.inner, v);
        Ok(())
    }

    #[inline]
    pub fn drop_items<const SIZE: usize>(&mut self) -> Result<(), TxScriptError> {
        match self.len() >= SIZE {
            true => {
                self.inner.truncate(self.len() - SIZE);
                Ok(())
            }
            false => Err(TxScriptError::InvalidStackOperation(SIZE, self.len())),
        }
    }

    #[inline]
    pub fn dup_items<const SIZE: usize>(&mut self) -> Result<(), TxScriptError> {
        match self.len() >= SIZE {
            true => {
                let added_bytes = total_bytes(&self.inner[self.len() - SIZE..]);
                self.add_pushed_bytes(added_bytes);
                self.inner.extend_from_within(self.len() - SIZE..);
                Ok(())
            }
            false => Err(TxScriptError::InvalidStackOperation(SIZE, self.len())),
        }
    }

    #[inline]
    pub fn over_items<const SIZE: usize>(&mut self) -> Result<(), TxScriptError> {
        match self.len() >= 2 * SIZE {
            true => {
                let added_bytes = total_bytes(&self.inner[self.len() - 2 * SIZE..self.len() - SIZE]);
                self.add_pushed_bytes(added_bytes);
                self.inner.extend_from_within(self.len() - 2 * SIZE..self.len() - SIZE);
                Ok(())
            }
            false => Err(TxScriptError::InvalidStackOperation(2 * SIZE, self.len())),
        }
    }

    #[inline]
    pub fn rot_items<const SIZE: usize>(&mut self) -> Result<(), TxScriptError> {
        match self.len() >= 3 * SIZE {
            true => {
                // Rotate the trailing `3 * SIZE` frame left by `SIZE`, moving the oldest group to the top.
                //
                // SIZE = 1:
                //   stack: [a, b, c, d, e, f]
                //                   [d, e, f]
                //                    << 1
                //       -> [a, b, c, e, f, d]
                //
                // SIZE = 2:
                //   stack: [a, b, c, d, e, f]
                //          [a, b, c, d, e, f]
                //           << 2
                //       -> [c, d, e, f, a, b]
                let len = self.len();
                self.inner[len - 3 * SIZE..].rotate_left(SIZE);
                Ok(())
            }
            false => Err(TxScriptError::InvalidStackOperation(3 * SIZE, self.len())),
        }
    }

    #[inline]
    pub fn swap_items<const SIZE: usize>(&mut self) -> Result<(), TxScriptError> {
        match self.len() >= 2 * SIZE {
            true => {
                // Rotate the trailing `2 * SIZE` frame left by `SIZE`, swapping the top two groups.
                //
                // SIZE = 1:
                //   stack: [a, b, c, d]
                //                [c, d]
                //                 << 1
                //       -> [a, b, d, c]
                //
                // SIZE = 2:
                //   stack: [a, b, c, d, e, f]
                //                [c, d, e, f]
                //                 << 2
                //       -> [a, b, e, f, c, d]
                let len = self.len();
                self.inner[len - 2 * SIZE..].rotate_left(SIZE);
                Ok(())
            }
            false => Err(TxScriptError::InvalidStackOperation(2 * SIZE, self.len())),
        }
    }

    #[inline]
    pub fn roll(&mut self, loc: usize) -> Result<(), TxScriptError> {
        if loc >= self.len() {
            return Err(TxScriptError::InvalidStackOperation(loc, self.len()));
        }
        if loc == 0 {
            return Ok(());
        }
        let from = self.len() - loc - 1;
        self.inner[from..].rotate_left(1);
        Ok(())
    }

    pub fn clear(&mut self) {
        self.inner.clear()
    }

    pub fn pop(&mut self) -> Result<StackEntry, TxScriptError> {
        self.inner.pop().ok_or(TxScriptError::EmptyStack)
    }

    pub fn split_off(&mut self, at: usize) -> Vec<StackEntry> {
        self.inner.split_off(at)
    }

    pub fn push(&mut self, item: StackEntry) -> Result<(), TxScriptError> {
        if item.len() > self.max_element_size() {
            return Err(TxScriptError::ElementTooBig(item.len(), self.max_element_size()));
        }
        self.add_pushed_bytes(item.len());
        self.inner.push(item);
        Ok(())
    }

    pub fn push_unmetered(&mut self, item: StackEntry) -> Result<(), TxScriptError> {
        if item.len() > self.max_element_size() {
            return Err(TxScriptError::ElementTooBig(item.len(), self.max_element_size()));
        }
        self.inner.push(item);
        Ok(())
    }

    pub fn remove(&mut self, index: usize) -> StackEntry {
        self.inner.remove(index)
    }
}

#[cfg(test)]
mod tests {
    use super::OpcodeData;
    use crate::data_stack::{SizedEncodeInt, Stack, serialize_i64};
    use kaspa_txscript_errors::{SerializationError, TxScriptError};
    use kaspa_utils::hex::FromHex;

    fn audit_stack(entries: usize) -> Vec<Vec<u8>> {
        (0..entries)
            .map(|index| {
                let len = index % 5;
                let mut item = vec![index as u8; len];
                item.push((index >> 8) as u8);
                item
            })
            .collect()
    }

    fn stack_to_vec(stack: &Stack) -> Vec<Vec<u8>> {
        stack.inner().iter().map(|entry| entry.to_vec()).collect()
    }

    fn master_reference_rot<const SIZE: usize>(stack: &mut Vec<Vec<u8>>) -> Result<(), TxScriptError> {
        match stack.len() >= 3 * SIZE {
            true => {
                let drained = stack.drain(stack.len() - 3 * SIZE..stack.len() - 2 * SIZE).collect::<Vec<Vec<u8>>>();
                stack.extend(drained);
                Ok(())
            }
            false => Err(TxScriptError::InvalidStackOperation(3 * SIZE, stack.len())),
        }
    }

    fn master_reference_swap<const SIZE: usize>(stack: &mut Vec<Vec<u8>>) -> Result<(), TxScriptError> {
        match stack.len() >= 2 * SIZE {
            true => {
                let drained = stack.drain(stack.len() - 2 * SIZE..stack.len() - SIZE).collect::<Vec<Vec<u8>>>();
                stack.extend(drained);
                Ok(())
            }
            false => Err(TxScriptError::InvalidStackOperation(2 * SIZE, stack.len())),
        }
    }

    fn old_roll_remove_push_reference(stack: &mut Vec<Vec<u8>>, loc: usize) -> Result<(), TxScriptError> {
        if loc >= stack.len() {
            return Err(TxScriptError::InvalidStackOperation(loc, stack.len()));
        }

        let item = stack.remove(stack.len() - loc - 1);
        stack.push(item);
        Ok(())
    }

    fn master_reference_serialize_i64(from: i64) -> Vec<u8> {
        let sign = from.signum();
        let mut positive = from.unsigned_abs();
        let mut last_saturated = false;
        let mut number_vec: Vec<u8> = std::iter::from_fn(move || {
            if positive == 0 {
                if last_saturated {
                    last_saturated = false;
                    Some(0)
                } else {
                    None
                }
            } else {
                let value = positive & 0xff;
                last_saturated = (value & 0x80) != 0;
                positive >>= 8;
                Some(value as u8)
            }
        })
        .collect();

        if sign == -1 {
            match number_vec.last_mut() {
                Some(num) => *num |= 0x80,
                _ => unreachable!(),
            }
        }
        number_vec
    }

    fn master_reference_check_minimal_data_encoding(v: &[u8]) -> Result<(), TxScriptError> {
        if v.is_empty() {
            return Ok(());
        }

        if v[v.len() - 1] & 0x7f == 0 && (v.len() == 1 || v[v.len() - 2] & 0x80 == 0) {
            return Err(TxScriptError::NotMinimalData(format!("numeric value encoded as {v:x?} is not minimally encoded")));
        }
        Ok(())
    }

    fn master_reference_deserialize_i64(v: &[u8]) -> Result<i64, TxScriptError> {
        match v.len() {
            l if l > size_of::<i64>() => {
                Err(TxScriptError::NotMinimalData(format!("numeric value encoded as {v:x?} is longer than 8 bytes")))
            }
            0 => Ok(0),
            _ => {
                master_reference_check_minimal_data_encoding(v)?;
                let msb = v[v.len() - 1];
                let sign = 1 - 2 * ((msb >> 7) as i64);
                let first_byte = (msb & 0x7f) as i64;
                Ok(v[..v.len() - 1].iter().rev().map(|v| *v as i64).fold(first_byte, |accum, item| (accum << 8) + item) * sign)
            }
        }
    }

    fn master_reference_deserialize_sized<const LEN: usize>(v: &[u8]) -> Result<SizedEncodeInt<LEN>, TxScriptError> {
        match v.len() > LEN {
            true => Err(TxScriptError::NumberTooBig(format!(
                "numeric value encoded as {:x?} is {} bytes which exceeds the max allowed of {}",
                v,
                v.len(),
                LEN
            ))),
            false => master_reference_deserialize_i64(v).map(SizedEncodeInt::<LEN>),
        }
    }

    fn audit_numeric_inputs() -> Vec<Vec<u8>> {
        let mut inputs = vec![vec![]];
        for len in 1..=9 {
            inputs.push(vec![0; len]);
            inputs.push(vec![1; len]);
            inputs.push(vec![0x7f; len]);
            inputs.push(vec![0x80; len]);
            inputs.push(vec![0xff; len]);

            let mut low_one = vec![0; len];
            low_one[0] = 1;
            inputs.push(low_one);

            let mut high_one = vec![0; len];
            high_one[len - 1] = 1;
            inputs.push(high_one);

            let mut sign_boundary = vec![0; len];
            sign_boundary[0] = 0x80;
            if len > 1 {
                sign_boundary[1] = 0x00;
            }
            inputs.push(sign_boundary);
        }
        inputs.sort();
        inputs.dedup();
        inputs
    }

    fn assert_rot_matches_master_reference<const SIZE: usize>() {
        for entries in 0..=64 {
            let initial = audit_stack(entries);
            let mut current = Stack::from(initial.clone());
            let mut reference = initial;

            let current_result = current.rot_items::<SIZE>();
            let reference_result = master_reference_rot::<SIZE>(&mut reference);

            assert_eq!(current_result, reference_result, "rot_items::<{SIZE}> result mismatch at stack len {entries}");
            assert_eq!(stack_to_vec(&current), reference, "rot_items::<{SIZE}> stack mismatch at stack len {entries}");
        }
    }

    fn assert_swap_matches_master_reference<const SIZE: usize>() {
        for entries in 0..=64 {
            let initial = audit_stack(entries);
            let mut current = Stack::from(initial.clone());
            let mut reference = initial;

            let current_result = current.swap_items::<SIZE>();
            let reference_result = master_reference_swap::<SIZE>(&mut reference);

            assert_eq!(current_result, reference_result, "swap_items::<{SIZE}> result mismatch at stack len {entries}");
            assert_eq!(stack_to_vec(&current), reference, "swap_items::<{SIZE}> stack mismatch at stack len {entries}");
        }
    }

    #[test]
    fn audit_canonical_number_serialization_matches_master_reference_algorithm() {
        let values = (-10_000i64..=10_000).chain([
            i64::MIN + 1,
            i64::MIN / 2,
            i32::MIN as i64 - 1,
            i32::MIN as i64,
            i32::MAX as i64,
            i32::MAX as i64 + 1,
            i64::MAX / 2,
            i64::MAX,
        ]);

        for value in values {
            let current = serialize_i64(value, None).map(|entry| entry.into_vec());
            let reference = master_reference_serialize_i64(value);

            assert_eq!(current, Ok(reference.clone()), "serialize_i64({value}) mismatch");
            assert_eq!(
                <Vec<u8> as OpcodeData<i64>>::serialize(&value),
                Ok(reference),
                "OpcodeData::<i64>::serialize({value}) mismatch"
            );
        }

        assert_eq!(
            <Vec<u8> as OpcodeData<i64>>::serialize(&i64::MIN),
            Err(SerializationError::NumberTooLong(i64::MIN, 8)),
            "i64::MIN must remain unrepresentable in canonical 8-byte script number encoding"
        );
    }

    #[test]
    fn audit_pre_activation_number_deserialization_matches_master_reference_algorithm() {
        for input in audit_numeric_inputs() {
            assert_eq!(
                <Vec<u8> as OpcodeData<i64>>::deserialize(&input, true),
                master_reference_deserialize_sized::<8>(&input).map(i64::from),
                "pre-activation i64 deserialize mismatch for {input:x?}"
            );
            assert_eq!(
                <Vec<u8> as OpcodeData<i32>>::deserialize(&input, true),
                master_reference_deserialize_sized::<4>(&input).map(|v| i32::try_from(v).expect("master i32 is within range")),
                "pre-activation i32 deserialize mismatch for {input:x?}"
            );
        }
    }

    #[test]
    fn audit_stack_pop_items_uses_master_canonical_rules_when_covenants_disabled() {
        for input in audit_numeric_inputs() {
            let prefix = vec![0xaa];
            let mut current = Stack::new(vec![prefix.clone().into(), input.clone().into()], false);
            let mut reference = vec![prefix.clone(), input.clone()];

            let current_result: Result<[i64; 1], TxScriptError> = current.pop_items();
            let reference_result = if reference.is_empty() {
                Err(TxScriptError::InvalidStackOperation(1, reference.len()))
            } else {
                let popped = reference.split_off(reference.len() - 1);
                master_reference_deserialize_sized::<8>(&popped[0]).map(|v| [i64::from(v)])
            };

            assert_eq!(current_result, reference_result, "pop_items::<i64> result mismatch for {input:x?}");
            assert_eq!(stack_to_vec(&current), reference, "pop_items::<i64> stack mutation mismatch for {input:x?}");
        }
    }

    #[test]
    fn audit_rot_items_matches_master_reference_algorithm() {
        assert_rot_matches_master_reference::<1>();
        assert_rot_matches_master_reference::<2>();
        assert_rot_matches_master_reference::<3>();
        assert_rot_matches_master_reference::<4>();
        assert_rot_matches_master_reference::<5>();
        assert_rot_matches_master_reference::<8>();
    }

    #[test]
    fn audit_swap_items_matches_master_reference_algorithm() {
        assert_swap_matches_master_reference::<1>();
        assert_swap_matches_master_reference::<2>();
        assert_swap_matches_master_reference::<3>();
        assert_swap_matches_master_reference::<4>();
        assert_swap_matches_master_reference::<5>();
        assert_swap_matches_master_reference::<8>();
    }

    #[test]
    fn audit_roll_matches_remove_then_push_reference_algorithm() {
        for entries in 0..=64 {
            for loc in 0..=entries + 2 {
                let initial = audit_stack(entries);
                let mut current = Stack::from(initial.clone());
                let mut reference = initial;

                let current_result = current.roll(loc);
                let reference_result = old_roll_remove_push_reference(&mut reference, loc);

                assert_eq!(current_result, reference_result, "roll({loc}) result mismatch at stack len {entries}");
                assert_eq!(stack_to_vec(&current), reference, "roll({loc}) stack mismatch at stack len {entries}");
            }
        }
    }

    // TestScriptNumBytes
    #[test]
    fn test_serialize() {
        struct TestCase {
            num: i64,
            serialized: Vec<u8>,
        }

        let tests = vec![
            TestCase { num: 0, serialized: vec![] },
            TestCase { num: 1, serialized: Vec::from_hex("01").expect("failed parsing hex") },
            TestCase { num: -1, serialized: Vec::from_hex("81").expect("failed parsing hex") },
            TestCase { num: 127, serialized: Vec::from_hex("7f").expect("failed parsing hex") },
            TestCase { num: -127, serialized: Vec::from_hex("ff").expect("failed parsing hex") },
            TestCase { num: 128, serialized: Vec::from_hex("8000").expect("failed parsing hex") },
            TestCase { num: -128, serialized: Vec::from_hex("8080").expect("failed parsing hex") },
            TestCase { num: 129, serialized: Vec::from_hex("8100").expect("failed parsing hex") },
            TestCase { num: -129, serialized: Vec::from_hex("8180").expect("failed parsing hex") },
            TestCase { num: 256, serialized: Vec::from_hex("0001").expect("failed parsing hex") },
            TestCase { num: -256, serialized: Vec::from_hex("0081").expect("failed parsing hex") },
            TestCase { num: 32767, serialized: Vec::from_hex("ff7f").expect("failed parsing hex") },
            TestCase { num: -32767, serialized: Vec::from_hex("ffff").expect("failed parsing hex") },
            TestCase { num: 32768, serialized: Vec::from_hex("008000").expect("failed parsing hex") },
            TestCase { num: -32768, serialized: Vec::from_hex("008080").expect("failed parsing hex") },
            TestCase { num: 65535, serialized: Vec::from_hex("ffff00").expect("failed parsing hex") },
            TestCase { num: -65535, serialized: Vec::from_hex("ffff80").expect("failed parsing hex") },
            TestCase { num: 524288, serialized: Vec::from_hex("000008").expect("failed parsing hex") },
            TestCase { num: -524288, serialized: Vec::from_hex("000088").expect("failed parsing hex") },
            TestCase { num: 7340032, serialized: Vec::from_hex("000070").expect("failed parsing hex") },
            TestCase { num: -7340032, serialized: Vec::from_hex("0000f0").expect("failed parsing hex") },
            TestCase { num: 8388608, serialized: Vec::from_hex("00008000").expect("failed parsing hex") },
            TestCase { num: -8388608, serialized: Vec::from_hex("00008080").expect("failed parsing hex") },
            TestCase { num: 2147483647, serialized: Vec::from_hex("ffffff7f").expect("failed parsing hex") },
            TestCase { num: -2147483647, serialized: Vec::from_hex("ffffffff").expect("failed parsing hex") },
            // Values that are out of range for data that is interpreted as
            // numbers before KIP-10 enabled, but are allowed as the result of numeric operations.
            TestCase { num: 2147483648, serialized: Vec::from_hex("0000008000").expect("failed parsing hex") },
            TestCase { num: -2147483648, serialized: Vec::from_hex("0000008080").expect("failed parsing hex") },
            TestCase { num: 2415919104, serialized: Vec::from_hex("0000009000").expect("failed parsing hex") },
            TestCase { num: -2415919104, serialized: Vec::from_hex("0000009080").expect("failed parsing hex") },
            TestCase { num: 4294967295, serialized: Vec::from_hex("ffffffff00").expect("failed parsing hex") },
            TestCase { num: -4294967295, serialized: Vec::from_hex("ffffffff80").expect("failed parsing hex") },
            TestCase { num: 4294967296, serialized: Vec::from_hex("0000000001").expect("failed parsing hex") },
            TestCase { num: -4294967296, serialized: Vec::from_hex("0000000081").expect("failed parsing hex") },
            TestCase { num: 281474976710655, serialized: Vec::from_hex("ffffffffffff00").expect("failed parsing hex") },
            TestCase { num: -281474976710655, serialized: Vec::from_hex("ffffffffffff80").expect("failed parsing hex") },
            TestCase { num: 72057594037927935, serialized: Vec::from_hex("ffffffffffffff00").expect("failed parsing hex") },
            TestCase { num: -72057594037927935, serialized: Vec::from_hex("ffffffffffffff80").expect("failed parsing hex") },
            TestCase { num: 9223372036854775807, serialized: Vec::from_hex("ffffffffffffff7f").expect("failed parsing hex") },
            TestCase { num: -9223372036854775807, serialized: Vec::from_hex("ffffffffffffffff").expect("failed parsing hex") },
        ];

        for test in tests {
            let serialized: Vec<u8> = OpcodeData::<i64>::serialize(&test.num).unwrap();
            assert_eq!(serialized, test.serialized);
            assert_eq!(serialize_i64(test.num, Some(test.serialized.len())).unwrap().into_vec(), test.serialized);
            if !test.serialized.is_empty() {
                serialize_i64(test.num, Some(test.serialized.len() - 1)).unwrap_err();
                // The default i64 serialization is minimal and cannot be encoded with less bytes.
            }
        }

        // special case 9-byte i64
        let r: Result<Vec<u8>, _> = OpcodeData::<i64>::serialize(&-9223372036854775808);
        assert_eq!(r, Err(SerializationError::NumberTooLong(-9223372036854775808, 8)));
    }

    // TestMakeScriptNum
    #[test]
    fn test_deserialize() {
        struct TestCase<T> {
            serialized: Vec<u8>,
            result: Result<T, TxScriptError>,
        }

        let tests = vec![
            TestCase::<i64> {
                serialized: Vec::from_hex("80").expect("failed parsing hex"),
                result: Err(TxScriptError::NotMinimalData("numeric value encoded as [80] is not minimally encoded".to_string())),
            },
            // Minimally encoded valid values with minimal encoding flag.
            // Should not error and return expected integral number.
            TestCase::<i64> { serialized: vec![], result: Ok(0) },
            TestCase::<i64> { serialized: Vec::from_hex("01").expect("failed parsing hex"), result: Ok(1) },
            TestCase::<i64> { serialized: Vec::from_hex("81").expect("failed parsing hex"), result: Ok(-1) },
            TestCase::<i64> { serialized: Vec::from_hex("7f").expect("failed parsing hex"), result: Ok(127) },
            TestCase::<i64> { serialized: Vec::from_hex("ff").expect("failed parsing hex"), result: Ok(-127) },
            TestCase::<i64> { serialized: Vec::from_hex("8000").expect("failed parsing hex"), result: Ok(128) },
            TestCase::<i64> { serialized: Vec::from_hex("8080").expect("failed parsing hex"), result: Ok(-128) },
            TestCase::<i64> { serialized: Vec::from_hex("8100").expect("failed parsing hex"), result: Ok(129) },
            TestCase::<i64> { serialized: Vec::from_hex("8180").expect("failed parsing hex"), result: Ok(-129) },
            TestCase::<i64> { serialized: Vec::from_hex("0001").expect("failed parsing hex"), result: Ok(256) },
            TestCase::<i64> { serialized: Vec::from_hex("0081").expect("failed parsing hex"), result: Ok(-256) },
            TestCase::<i64> { serialized: Vec::from_hex("ff7f").expect("failed parsing hex"), result: Ok(32767) },
            TestCase::<i64> { serialized: Vec::from_hex("ffff").expect("failed parsing hex"), result: Ok(-32767) },
            TestCase::<i64> { serialized: Vec::from_hex("008000").expect("failed parsing hex"), result: Ok(32768) },
            TestCase::<i64> { serialized: Vec::from_hex("008080").expect("failed parsing hex"), result: Ok(-32768) },
            TestCase::<i64> { serialized: Vec::from_hex("ffff00").expect("failed parsing hex"), result: Ok(65535) },
            TestCase::<i64> { serialized: Vec::from_hex("ffff80").expect("failed parsing hex"), result: Ok(-65535) },
            TestCase::<i64> { serialized: Vec::from_hex("000008").expect("failed parsing hex"), result: Ok(524288) },
            TestCase::<i64> { serialized: Vec::from_hex("000088").expect("failed parsing hex"), result: Ok(-524288) },
            TestCase::<i64> { serialized: Vec::from_hex("000070").expect("failed parsing hex"), result: Ok(7340032) },
            TestCase::<i64> { serialized: Vec::from_hex("0000f0").expect("failed parsing hex"), result: Ok(-7340032) },
            TestCase::<i64> { serialized: Vec::from_hex("00008000").expect("failed parsing hex"), result: Ok(8388608) },
            TestCase::<i64> { serialized: Vec::from_hex("00008080").expect("failed parsing hex"), result: Ok(-8388608) },
            TestCase::<i64> { serialized: Vec::from_hex("ffffff7f").expect("failed parsing hex"), result: Ok(2147483647) },
            TestCase::<i64> { serialized: Vec::from_hex("ffffffff").expect("failed parsing hex"), result: Ok(-2147483647) },
            // Non-minimally encoded, but otherwise valid values with
            // minimal encoding flag. Should error and return 0.
            TestCase::<i64> {
                serialized: Vec::from_hex("00").expect("failed parsing hex"),
                result: Err(TxScriptError::NotMinimalData("numeric value encoded as [0] is not minimally encoded".to_string())),
            }, // 0
            TestCase::<i64> {
                serialized: Vec::from_hex("0100").expect("failed parsing hex"),
                result: Err(TxScriptError::NotMinimalData("numeric value encoded as [1, 0] is not minimally encoded".to_string())),
            }, // 1
            TestCase::<i64> {
                serialized: Vec::from_hex("7f00").expect("failed parsing hex"),
                result: Err(TxScriptError::NotMinimalData("numeric value encoded as [7f, 0] is not minimally encoded".to_string())),
            }, // 127
            TestCase::<i64> {
                serialized: Vec::from_hex("800000").expect("failed parsing hex"),
                result: Err(TxScriptError::NotMinimalData("numeric value encoded as [80, 0, 0] is not minimally encoded".to_string())),
            }, // 128
            TestCase::<i64> {
                serialized: Vec::from_hex("810000").expect("failed parsing hex"),
                result: Err(TxScriptError::NotMinimalData("numeric value encoded as [81, 0, 0] is not minimally encoded".to_string())),
            }, // 129
            TestCase::<i64> {
                serialized: Vec::from_hex("000100").expect("failed parsing hex"),
                result: Err(TxScriptError::NotMinimalData("numeric value encoded as [0, 1, 0] is not minimally encoded".to_string())),
            }, // 256
            TestCase::<i64> {
                serialized: Vec::from_hex("ff7f00").expect("failed parsing hex"),
                result: Err(TxScriptError::NotMinimalData(
                    "numeric value encoded as [ff, 7f, 0] is not minimally encoded".to_string(),
                )),
            }, // 32767
            TestCase::<i64> {
                serialized: Vec::from_hex("00800000").expect("failed parsing hex"),
                result: Err(TxScriptError::NotMinimalData(
                    "numeric value encoded as [0, 80, 0, 0] is not minimally encoded".to_string(),
                )),
            }, // 32768
            TestCase::<i64> {
                serialized: Vec::from_hex("ffff0000").expect("failed parsing hex"),
                result: Err(TxScriptError::NotMinimalData(
                    "numeric value encoded as [ff, ff, 0, 0] is not minimally encoded".to_string(),
                )),
            }, // 65535
            TestCase::<i64> {
                serialized: Vec::from_hex("00000800").expect("failed parsing hex"),
                result: Err(TxScriptError::NotMinimalData(
                    "numeric value encoded as [0, 0, 8, 0] is not minimally encoded".to_string(),
                )),
            }, // 524288
            TestCase::<i64> {
                serialized: Vec::from_hex("00007000").expect("failed parsing hex"),
                result: Err(TxScriptError::NotMinimalData(
                    "numeric value encoded as [0, 0, 70, 0] is not minimally encoded".to_string(),
                )),
            }, // 7340032
               // Values above 8 bytes should always return error
        ];
        let kip10_tests = vec![
            TestCase::<i64> { serialized: Vec::from_hex("0000008000").expect("failed parsing hex"), result: Ok(2147483648i64) },
            TestCase::<i64> { serialized: Vec::from_hex("0000008080").expect("failed parsing hex"), result: Ok(-2147483648i64) },
            TestCase::<i64> { serialized: Vec::from_hex("0000009000").expect("failed parsing hex"), result: Ok(2415919104i64) },
            TestCase::<i64> { serialized: Vec::from_hex("0000009080").expect("failed parsing hex"), result: Ok(-2415919104i64) },
            TestCase::<i64> { serialized: Vec::from_hex("ffffffff00").expect("failed parsing hex"), result: Ok(4294967295i64) },
            TestCase::<i64> { serialized: Vec::from_hex("ffffffff80").expect("failed parsing hex"), result: Ok(-4294967295i64) },
            TestCase::<i64> { serialized: Vec::from_hex("0000000001").expect("failed parsing hex"), result: Ok(4294967296i64) },
            TestCase::<i64> { serialized: Vec::from_hex("0000000081").expect("failed parsing hex"), result: Ok(-4294967296i64) },
            TestCase::<i64> {
                serialized: Vec::from_hex("ffffffffffff00").expect("failed parsing hex"),
                result: Ok(281474976710655i64),
            },
            TestCase::<i64> {
                serialized: Vec::from_hex("ffffffffffff80").expect("failed parsing hex"),
                result: Ok(-281474976710655i64),
            },
            TestCase::<i64> {
                serialized: Vec::from_hex("ffffffffffffff00").expect("failed parsing hex"),
                result: Ok(72057594037927935i64),
            },
            TestCase::<i64> {
                serialized: Vec::from_hex("ffffffffffffff80").expect("failed parsing hex"),
                result: Ok(-72057594037927935i64),
            },
            TestCase::<i64> {
                serialized: Vec::from_hex("ffffffffffffff7f").expect("failed parsing hex"),
                result: Ok(9223372036854775807i64),
            },
            TestCase::<i64> {
                serialized: Vec::from_hex("ffffffffffffffff").expect("failed parsing hex"),
                result: Ok(-9223372036854775807i64),
            },
            // Minimally encoded values that are out of range for data that
            // is interpreted as script numbers with the minimal encoding
            // flag set. Should error and return 0.
            TestCase::<i64> {
                serialized: Vec::from_hex("000000000000008080").expect("failed parsing hex"),
                result: Err(TxScriptError::NumberTooBig(
                    "numeric value encoded as [0, 0, 0, 0, 0, 0, 0, 80, 80] is 9 bytes which exceeds the max allowed of 8".to_string(),
                )),
            },
        ];
        let test_of_size_5 = vec![
            TestCase::<SizedEncodeInt<5>> {
                serialized: Vec::from_hex("ffffffff7f").expect("failed parsing hex"),
                result: Ok(SizedEncodeInt::<5>(549755813887)),
            },
            TestCase::<SizedEncodeInt<5>> {
                serialized: Vec::from_hex("ffffffffff").expect("failed parsing hex"),
                result: Ok(SizedEncodeInt::<5>(-549755813887)),
            },
            TestCase::<SizedEncodeInt<5>> {
                serialized: Vec::from_hex("0009000100").expect("failed parsing hex"),
                result: Err(TxScriptError::NotMinimalData(
                    "numeric value encoded as [0, 9, 0, 1, 0] is not minimally encoded".to_string(),
                )),
            }, // 16779520
        ];

        let test_of_size_8 = vec![
            TestCase::<SizedEncodeInt<8>> {
                serialized: Vec::from_hex("ffffffffffffff7f").expect("failed parsing hex"),
                result: Ok(SizedEncodeInt::<8>(i64::MAX)),
            },
            TestCase::<SizedEncodeInt<8>> {
                serialized: Vec::from_hex("ffffffffffffffff").expect("failed parsing hex"),
                result: Ok(SizedEncodeInt::<8>(i64::MIN + 1)),
            },
        ];

        let test_of_size_9 = vec![
            TestCase::<SizedEncodeInt<9>> {
                serialized: Vec::from_hex("ffffffffffffffffff").expect("failed parsing hex"),
                result: Err(TxScriptError::NotMinimalData(
                    "numeric value encoded as [ff, ff, ff, ff, ff, ff, ff, ff, ff] is longer than 8 bytes".to_string(),
                )),
            },
            TestCase::<SizedEncodeInt<9>> {
                serialized: Vec::from_hex("ffffffffffffffff").expect("failed parsing hex"),
                result: Ok(SizedEncodeInt::<9>(i64::MIN + 1)),
            },
        ];

        let test_of_size_10 = vec![TestCase::<SizedEncodeInt<10>> {
            serialized: Vec::from_hex("00000000000000000000").expect("failed parsing hex"),
            result: Err(TxScriptError::NotMinimalData(
                "numeric value encoded as [0, 0, 0, 0, 0, 0, 0, 0, 0, 0] is longer than 8 bytes".to_string(),
            )),
        }];

        let test_bool = vec![
            TestCase::<bool> { serialized: Vec::from_hex("").expect("failed parsing hex"), result: Ok(false) },
            TestCase::<bool> { serialized: Vec::from_hex("00").expect("failed parsing hex"), result: Ok(false) },
            TestCase::<bool> { serialized: Vec::from_hex("0000").expect("failed parsing hex"), result: Ok(false) },
            TestCase::<bool> { serialized: Vec::from_hex("0011").expect("failed parsing hex"), result: Ok(true) },
            TestCase::<bool> { serialized: Vec::from_hex("80").expect("failed parsing hex"), result: Ok(false) }, // Negative zero
            TestCase::<bool> { serialized: Vec::from_hex("8011").expect("failed parsing hex"), result: Ok(true) }, // MSB by itself is negative zero, but the whole number isn't
            TestCase::<bool> { serialized: Vec::from_hex("8080").expect("failed parsing hex"), result: Ok(true) }, // All bytes are negative zeroes by themselves, but the whole number isn't
            TestCase::<bool> { serialized: Vec::from_hex("1234").expect("failed parsing hex"), result: Ok(true) },
            TestCase::<bool> { serialized: Vec::from_hex("ffffffff").expect("failed parsing hex"), result: Ok(true) },
        ];

        for test in tests {
            // Ensure the error code is of the expected type and the error
            // code matches the value specified in the test instance.
            assert_eq!(test.serialized.deserialize(true), test.result);
        }

        for test in test_of_size_5 {
            // Ensure the error code is of the expected type and the error
            // code matches the value specified in the test instance.
            assert_eq!(test.serialized.deserialize(true), test.result);
        }

        for test in test_of_size_8 {
            // Ensure the error code is of the expected type and the error
            // code matches the value specified in the test instance.
            assert_eq!(test.serialized.deserialize(true), test.result);
        }

        for test in test_of_size_9 {
            // Ensure the error code is of the expected type and the error
            // code matches the value specified in the test instance.
            assert_eq!(test.serialized.deserialize(true), test.result);
        }

        for test in test_of_size_10 {
            // Ensure the error code is of the expected type and the error
            // code matches the value specified in the test instance.
            assert_eq!(test.serialized.deserialize(true), test.result);
        }

        for test in test_bool {
            // Ensure the error code is of the expected type and the error
            // code matches the value specified in the test instance.
            assert_eq!(test.serialized.deserialize(true), test.result);
        }
        for test in kip10_tests {
            // Ensure the error code is of the expected type and the error
            // code matches the value specified in the test instance.
            assert_eq!(test.serialized.deserialize(true), test.result);
        }
    }
}
