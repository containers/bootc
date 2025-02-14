/// Given an iterator that's clonable, split it into two iterators
/// at a given maximum number of elements.
pub fn iterator_split<I>(
    it: I,
    max: usize,
) -> (impl Iterator<Item = I::Item>, impl Iterator<Item = I::Item>)
where
    I: Iterator + Clone,
{
    let rest = it.clone();
    (it.take(max), rest.skip(max))
}

/// Given an iterator that's clonable, split off the first N elements
/// at the pivot point. If the iterator would be empty, return None.
/// Return the count of the remainder.
pub fn iterator_split_nonempty_rest_count<I>(
    it: I,
    max: usize,
) -> Option<(impl Iterator<Item = I::Item>, usize)>
where
    I: Iterator + Clone,
{
    let rest = it.clone();
    let mut it = it.peekable();
    if it.peek().is_some() {
        Some((it.take(max), rest.skip(max).count()))
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_it_split() {
        let a: &[&str] = &[];
        for v in [0, 1, 5] {
            let (first, rest) = iterator_split(a.iter(), v);
            assert_eq!(first.count(), 0);
            assert_eq!(rest.count(), 0);
        }
        let a = &["foo"];
        for v in [1, 5] {
            let (first, rest) = iterator_split(a.iter(), v);
            assert_eq!(first.count(), 1);
            assert_eq!(rest.count(), 0);
        }
        let (first, rest) = iterator_split(a.iter(), 1);
        assert_eq!(first.count(), 1);
        assert_eq!(rest.count(), 0);
        let a = &["foo", "bar", "baz", "blah", "other"];
        let (first, rest) = iterator_split(a.iter(), 2);
        assert_eq!(first.count(), 2);
        assert_eq!(rest.count(), 3);
    }

    #[test]
    fn test_split_empty_iterator() {
        let a: &[&str] = &[];
        for v in [0, 1, 5] {
            assert!(iterator_split_nonempty_rest_count(a.iter(), v).is_none());
        }
    }

    #[test]
    fn test_split_nonempty_iterator() {
        let a = &["foo"];

        let Some((elts, 1)) = iterator_split_nonempty_rest_count(a.iter(), 0) else {
            panic!()
        };
        assert_eq!(elts.count(), 0);

        let Some((elts, 0)) = iterator_split_nonempty_rest_count(a.iter(), 1) else {
            panic!()
        };
        assert_eq!(elts.count(), 1);

        let Some((elts, 0)) = iterator_split_nonempty_rest_count(a.iter(), 5) else {
            panic!()
        };
        assert_eq!(elts.count(), 1);

        let a = &["foo", "bar", "baz", "blah", "other"];
        let Some((elts, 3)) = iterator_split_nonempty_rest_count(a.iter(), 2) else {
            panic!()
        };
        assert_eq!(elts.count(), 2);
    }
}
