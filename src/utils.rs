pub struct BatchIter<T> {
    inner: T,
    batch_size: usize,
}

impl<T> BatchIter<T>
where
    T: Iterator,
{
    pub fn new(inner: T, batch_size: usize) -> BatchIter<T> {
        BatchIter { inner, batch_size }
    }
}

impl<T> Iterator for BatchIter<T>
where
    T: Iterator,
{
    type Item = Vec<T::Item>;

    fn next(&mut self) -> Option<Self::Item> {
        let mut batch = Vec::with_capacity(self.batch_size);
        for _ in 0..self.batch_size {
            match self.inner.next() {
                Some(inner_item) => batch.push(inner_item),
                None => break,
            }
        };
        if batch.is_empty() {
            None
        } else {
            Some(batch)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_iter() {
        let v = vec![1, 2, 3, 4, 5, 6, 7, 8, 9, 10];
        let mut iter = BatchIter::new(v.into_iter(), 4);
        assert_eq!(iter.next(), Some(vec![1, 2, 3, 4]));
        assert_eq!(iter.next(), Some(vec![5, 6, 7, 8]));
        assert_eq!(iter.next(), Some(vec![9, 10]));
        assert_eq!(iter.next(), None);
    }
}
