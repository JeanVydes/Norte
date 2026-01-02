use arrayvec::ArrayVec;
use core::mem::replace;

#[derive(Debug, Clone)]
pub struct LRUCache<T, const N: usize> {
    entries: ArrayVec<Entry<T>, N>,
    head: u16,
    tail: u16,
}

#[derive(Debug, Clone)]
struct Entry<T> {
    val: T,
    prev: u16,
    next: u16,
}

impl<T, const N: usize> Default for LRUCache<T, N> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T, const N: usize> LRUCache<T, N> {
    pub const fn new() -> Self {
        assert!(N < u16::MAX as usize, "Capacity overflow");
        LRUCache {
            entries: ArrayVec::new_const(),
            head: 0,
            tail: 0,
        }
    }

    pub fn insert(&mut self, val: T) -> Option<T> {
        let new_entry = Entry {
            val,
            prev: 0,
            next: 0,
        };

        // If the cache is full, replace the oldest entry. Otherwise, add an entry.
        if self.entries.is_full() {
            let i = self.pop_back();
            let old_entry = replace(self.entry(i), new_entry);
            self.push_front(i);
            Some(old_entry.val)
        } else {
            let i = self.entries.len() as u16;
            self.entries.push(new_entry);
            self.push_front(i);
            None
        }
    }

    /// Returns the first item in the cache that matches the given predicate.
    /// Touches the result (makes it most-recently-used) on a hit.
    pub fn find<F>(&mut self, pred: F) -> Option<&mut T>
    where
        F: FnMut(&T) -> bool,
    {
        if self.touch(pred) {
            self.front_mut()
        } else {
            None
        }
    }

    /// Performs a lookup on the cache with the given test routine. Touches
    /// the result on a hit.
    pub fn lookup<F, R>(&mut self, mut pred: F) -> Option<R>
    where
        F: FnMut(&mut T) -> Option<R>,
    {
        let mut iter = self.iter_mut();
        while let Some((i, val)) = iter.next() {
            if let Some(r) = pred(val) {
                self.touch_index(i);
                return Some(r);
            }
        }
        None
    }

    /// Mutable iterator over all items in the cache that match the given predicate.
    pub fn iter_mut_all<F, R>(&mut self, mut pred: F) -> Vec<R>
    where
        F: FnMut(&mut T) -> Option<R>,
    {
        let mut results = Vec::new();
        let mut iter = self.iter_mut();
        while let Some((_i, val)) = iter.next() {
            if let Some(r) = pred(val) {
                results.push(r);
            }
        }
        results
    }

    /// Immutable iterator over all items in the cache that match the given predicate.
    pub fn find_all<F>(&self, mut pred: F) -> Vec<&T>
    where
        F: FnMut(&T) -> bool,
    {
        let mut results = Vec::new();
        for val in self.iter() {
            if pred(val) {
                results.push(val);
            }
        }
        results
    }

    // Immutable lookup over all items in the cache that match the given predicate.
    pub fn lookup_all<F, R>(&self, mut pred: F) -> Vec<R>
    where
        F: FnMut(&T) -> Option<R>,
    {
        let mut results = Vec::new();
        for val in self.iter() {
            if let Some(r) = pred(val) {
                results.push(r);
            }
        }
        results
    }

    /// Returns the number of elements in the cache.
    #[inline]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Returns true if the cache is empty.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Evict all elements from the cache.
    #[inline]
    pub fn clear(&mut self) {
        self.entries.clear();
    }

    /// Returns the front entry in the list (most recently used).
    pub fn front(&self) -> Option<&T> {
        self.entries.get(self.head as usize).map(|e| &e.val)
    }

    /// Returns a mutable reference to the front entry in the list (most recently used).
    pub fn front_mut(&mut self) -> Option<&mut T> {
        self.entries.get_mut(self.head as usize).map(|e| &mut e.val)
    }

    /// Returns the n-th entry in the list (most recently used).
    pub fn get(&self, index: usize) -> Option<&T> {
        self.iter().nth(index)
    }

    /// Touches the first item in the cache that matches the given predicate (marks it as
    /// most-recently-used).
    /// Returns `true` on a hit, `false` if no matches.
    pub fn touch<F>(&mut self, mut pred: F) -> bool
    where
        F: FnMut(&T) -> bool,
    {
        let mut iter = self.iter_mut();
        while let Some((i, val)) = iter.next() {
            if pred(val) {
                self.touch_index(i);
                return true;
            }
        }
        false
    }

    /// Iterate over the contents of this cache in order from most-recently-used to
    /// least-recently-used.
    pub fn iter(&self) -> Iter<'_, T, N> {
        Iter {
            pos: self.head,
            cache: self,
        }
    }

    pub fn capacity(&self) -> usize {
        N
    }

    /// Iterate mutably over the contents of this cache in order from most-recently-used to
    /// least-recently-used.
    fn iter_mut(&mut self) -> IterMut<'_, T, N> {
        IterMut {
            pos: self.head,
            cache: self,
        }
    }

    /// Touch a given entry, putting it first in the list.
    #[inline]
    fn touch_index(&mut self, i: u16) {
        if i != self.head {
            self.remove(i);
            self.push_front(i);
        }
    }

    #[inline(always)]
    fn entry(&mut self, i: u16) -> &mut Entry<T> {
        &mut self.entries[i as usize]
    }

    /// Remove an entry from the linked list.
    ///
    /// Note: This only unlinks the entry from the list; it does not remove it from the array.
    fn remove(&mut self, i: u16) {
        let prev = self.entry(i).prev;
        let next = self.entry(i).next;

        if i == self.head {
            self.head = next;
        } else {
            self.entry(prev).next = next;
        }

        if i == self.tail {
            self.tail = prev;
        } else {
            self.entry(next).prev = prev;
        }
    }

    /// Insert a new entry at the head of the list.
    pub fn push_front(&mut self, i: u16) {
        if self.entries.len() == 1 {
            self.tail = i;
        } else {
            self.entry(i).next = self.head;
            self.entry(self.head).prev = i;
        }
        self.head = i;
    }

    /// Remove the last entry from the linked list. Returns the index of the removed entry.
    ///
    /// Note: This only unlinks the entry from the list; it does not remove it from the array.
    fn pop_back(&mut self) -> u16 {
        let new_tail = self.entry(self.tail).prev;
        replace(&mut self.tail, new_tail)
    }
}

/// Mutable iterator over values in an `LRUCache`, from most-recently-used to least-recently-used.
struct IterMut<'a, T, const N: usize> {
    cache: &'a mut LRUCache<T, N>,
    pos: u16,
}

impl<'a, T, const N: usize> IterMut<'a, T, N> {
    fn next(&mut self) -> Option<(u16, &mut T)> {
        let index = self.pos;
        let entry = self.cache.entries.get_mut(index as usize)?;

        self.pos = if index == self.cache.tail {
            N as u16 // Point past the end of the array to signal we are done.
        } else {
            entry.next
        };
        Some((index, &mut entry.val))
    }
}

/// Iterator over values in an [`LRUCache`], from most-recently-used to least-recently-used.
pub struct Iter<'a, T, const N: usize> {
    cache: &'a LRUCache<T, N>,
    pos: u16,
}

impl<'a, T, const N: usize> Iterator for Iter<'a, T, N> {
    type Item = &'a T;

    fn next(&mut self) -> Option<&'a T> {
        let entry = self.cache.entries.get(self.pos as usize)?;

        self.pos = if self.pos == self.cache.tail {
            N as u16 // Point past the end of the array to signal we are done.
        } else {
            entry.next
        };
        Some(&entry.val)
    }
}
