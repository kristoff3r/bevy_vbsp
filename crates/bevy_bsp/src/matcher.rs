pub trait IntoStringMatcher<'a>: Clone {
    fn match_indices(self, needle: &'a str) -> impl Iterator<Item = (usize, &'a str)>;
}

impl<'a> IntoStringMatcher<'a> for &'static str {
    fn match_indices(self, needle: &'a str) -> impl Iterator<Item = (usize, &'a str)> {
        needle.match_indices(self)
    }
}

#[derive(Debug, Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct AnyString;

impl<'this> IntoStringMatcher<'this> for AnyString {
    fn match_indices(self, needle: &'this str) -> impl Iterator<Item = (usize, &'this str)> {
        std::iter::once((0, needle))
    }
}

pub struct AnyOf<T>(pub T);

impl<'a, T> IntoStringMatcher<'a> for &'a AnyOf<T>
where
    T: 'a,
    &'a T: IntoIterator,
    <&'a T as IntoIterator>::IntoIter: 'a,
    <&'a T as IntoIterator>::Item: IntoStringMatcher<'a> + 'a,
{
    fn match_indices(self, needle: &'a str) -> impl Iterator<Item = (usize, &'a str)> {
        self.0
            .into_iter()
            .flat_map(|matcher| matcher.match_indices(needle))
    }
}

impl<'a, T> IntoStringMatcher<'a> for &'a T
where
    T: IntoStringMatcher<'a>,
{
    fn match_indices(self, needle: &'a str) -> impl Iterator<Item = (usize, &'a str)> {
        (*self).clone().match_indices(needle)
    }
}

#[derive(Clone)]
pub struct StartsWith<T: ?Sized>(pub T);

pub trait StringMatcher {
    fn is_match(&self, needle: &str) -> bool;
}

pub struct Not<T>(pub T);

impl<T> StringMatcher for Not<T>
where
    T: StringMatcher,
{
    fn is_match(&self, needle: &str) -> bool {
        !self.0.is_match(needle)
    }
}

impl<T> StringMatcher for T
where
    T: for<'a> IntoStringMatcher<'a>,
{
    fn is_match(&self, needle: &str) -> bool {
        self.match_indices(needle)
            .any(|(_, str)| str.len() == needle.len())
    }
}

impl<'a, T> IntoStringMatcher<'a> for StartsWith<T>
where
    T: IntoStringMatcher<'a>,
{
    fn match_indices(self, needle: &'a str) -> impl Iterator<Item = (usize, &'a str)> {
        self.0
            .match_indices(needle)
            .filter_map(move |(i, _)| (i == 0).then_some((0, needle)))
    }
}
