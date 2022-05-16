use std::{num::NonZeroU32, ops::Deref};

use super::*;
use crate::enc::SplitView;

mod internal {
    use super::*;

    pub trait Lens<'a> {
        type Target: ?Sized;
        /// Views the target of a `View<Self>` as `&Self`.
        fn view(target: &Self::Target) -> &Self;
    }

    impl<'a> Lens<'a> for EStr {
        type Target = [u8];
        #[inline]
        fn view(bytes: &[u8]) -> &Self {
            // SAFETY: `Self::new` ensures that the bytes are properly encoded.
            unsafe { EStr::new_unchecked(bytes) }
        }
    }

    impl<'a> Lens<'a> for str {
        type Target = [u8];
        #[inline]
        fn view(bytes: &[u8]) -> &Self {
            // SAFETY: `Self::new` ensures that the bytes are valid UTF-8.
            unsafe { str::from_utf8_unchecked(bytes) }
        }
    }

    impl<'a> Lens<'a> for Scheme {
        type Target = [u8];
        #[inline]
        fn view(bytes: &[u8]) -> &Self {
            Scheme::new(str::view(bytes))
        }
    }

    impl<'i, 'a> Lens<'i> for Authority<&'a mut [u8]> {
        type Target = Uri<&'a mut [u8]>;
        #[inline]
        fn view(uri: &Self::Target) -> &Self {
            // SAFETY: `Self::new` ensures that the authority is present and not modified.
            unsafe { Authority::new(uri) }
        }
    }

    impl<'i, 'a> Lens<'i> for Host<&'a mut [u8]> {
        type Target = Authority<&'a mut [u8]>;
        #[inline]
        fn view(auth: &Self::Target) -> &Self {
            // SAFETY: `Self::new` ensures that the host is not modified.
            unsafe { Host::new(auth) }
        }
    }

    impl<'a> Lens<'a> for Path {
        type Target = [u8];
        #[inline]
        fn view(bytes: &[u8]) -> &Self {
            Path::new(EStr::view(bytes))
        }
    }
}

pub(crate) use self::internal::Lens;

/// A smart pointer that allows viewing a mutable byte slice as `&T`.
///
/// This struct was introduced considering the fact that a bare `&mut EStr` wouldn't
/// do for in-place decoding because such decoding breaks the invariant of [`EStr`].
///
/// Six types of *lenses* may be used as `T`: [`EStr`], [`prim@str`], [`Scheme`],
/// [`Authority`], [`Host`], and [`Path`].
pub struct View<'a, T: ?Sized + Lens<'a>>(&'a mut T::Target, PhantomData<&'a T>);

impl<'a, T: ?Sized + Lens<'a>> Deref for View<'a, T> {
    type Target = T;
    #[inline]
    fn deref(&self) -> &T {
        T::view(self.0)
    }
}

impl<'a, T: ?Sized + Lens<'a>> AsRef<T> for View<'a, T> {
    #[inline]
    fn as_ref(&self) -> &T {
        self
    }
}

impl<'a, T: ?Sized + Lens<'a>> View<'a, T> {
    /// Creates a `View<T>` from its target assuming validity.
    ///
    /// # Safety
    ///
    /// The target must be valid as `T`.
    #[inline]
    pub(crate) unsafe fn new(target: &'a mut T::Target) -> Self {
        View(target, PhantomData)
    }

    /// Consumes this `View` and yields the underlying `&T`.
    #[inline]
    pub fn into_ref(self) -> &'a T {
        T::view(self.0)
    }
}

impl<'a, T: ?Sized + Lens<'a, Target = [u8]>> View<'a, T> {
    /// Consumes this `View` and yields the underlying mutable byte slice.
    ///
    /// This method is only available for lenses [`EStr`], [`prim@str`], [`Scheme`], and [`Path`].
    #[inline]
    pub fn into_bytes(self) -> &'a mut [u8] {
        self.0
    }
}

/// A [`Scheme`] view into a mutable byte slice that allows lowercasing in-place.
impl<'a> View<'a, Scheme> {
    /// Converts the scheme to lower case in-place.
    ///
    /// # Examples
    ///
    /// ```
    /// use fluent_uri::Uri;
    ///
    /// let mut bytes = *b"HTTP://example.com/";
    /// let mut uri = Uri::parse_mut(&mut bytes)?;
    ///
    /// let mut scheme = uri.take_scheme().unwrap();
    /// scheme.make_lowercase();
    /// assert_eq!(scheme.as_str(), "http");
    /// # Ok::<_, fluent_uri::ParseError>(())
    /// ```
    #[inline]
    pub fn make_lowercase(&mut self) {
        // SAFETY: Setting the sixth bit keeps UTF-8.
        for byte in &mut *self.0 {
            *byte |= ASCII_CASE_MASK;
        }
    }
}

/// An [`Authority`] view into a mutable byte slice.
impl<'i, 'a> View<'i, Authority<&'a mut [u8]>> {
    /// Consumes this `View<Authority>` and yields the underlying `View<str>`.
    ///
    /// The userinfo or port subcomponent is truncated if it is already taken.
    ///
    /// # Panics
    ///
    /// Panics if the host subcomponent is already taken.
    #[inline]
    pub fn into_str_view(self) -> View<'a, str> {
        if self.uri.tag.contains(Tag::HOST_TAKEN) {
            component_taken();
        }

        let end = if self.uri.tag.contains(Tag::PORT_TAKEN) {
            self.host_bounds().1
        } else {
            self.uri.path_bounds.0
        };

        // SAFETY: The indexes are within bounds and the validation is done.
        unsafe { self.0.view(self.start(), end) }
    }

    /// Takes a view of the userinfo subcomponent, leaving a `None` in its place.
    #[inline]
    pub fn take_userinfo(&mut self) -> Option<View<'a, EStr>> {
        let (start, host_start) = (self.start(), self.host_bounds().0);
        (start != host_start).then(|| unsafe {
            // SAFETY: Host won't start at index 0.
            self.data().start.set(NonZeroU32::new_unchecked(host_start));
            // SAFETY: The indexes are within bounds and the validation is done.
            self.0.view(start, host_start - 1)
        })
    }

    /// Takes a view of the host subcomponent.
    ///
    /// # Panics
    ///
    /// Panics if the host subcomponent is already taken.
    #[inline]
    pub fn take_host(&mut self) -> View<'i, Host<&'a mut [u8]>> {
        if self.uri.tag.contains(Tag::HOST_TAKEN) {
            component_taken();
        }
        self.0.tag |= Tag::HOST_TAKEN;

        // SAFETY: Transparency holds.
        unsafe { View::new(&mut *(self.0 as *mut Uri<_> as *mut Authority<_>)) }
    }

    /// Takes a view of the port subcomponent, leaving a `None` in its place.
    #[inline]
    pub fn take_port(&mut self) -> Option<View<'a, str>> {
        if self.uri.tag.contains(Tag::PORT_TAKEN) {
            return None;
        }
        self.0.tag |= Tag::PORT_TAKEN;

        let (host_end, end) = (self.host_bounds().1, self.uri.path_bounds.0);
        // SAFETY: The indexes are within bounds and the validation is done.
        (host_end != end).then(|| unsafe { self.0.view(host_end + 1, end) })
    }
}

/// A [`Host`] view into a mutable byte slice.
impl<'i, 'a> View<'i, Host<&'a mut [u8]>> {
    /// Consumes this `View<Host>` and yields the underlying `View<str>`.
    #[inline]
    pub fn into_str_view(self) -> View<'a, str> {
        // SAFETY: The indexes are within bounds and the validation is done.
        unsafe { self.0.uri.view(self.bounds().0, self.bounds().1) }
    }

    /// Consumes this `View<Host>` and yields the underlying `View<EStr>`,
    /// assuming that the host is a registered name.
    ///
    /// # Panics
    ///
    /// Panics if the host is not a registered name.
    #[inline]
    pub fn unwrap_reg_name(self) -> View<'a, EStr> {
        assert!(self.0.uri.tag.contains(Tag::HOST_REG_NAME));
        // SAFETY: The indexes are within bounds and the validation is done.
        unsafe { self.0.uri.view(self.bounds().0, self.bounds().1) }
    }
}

/// A [`Path`] view into a mutable byte slice.
impl<'a> View<'a, Path> {
    /// Consumes this `View<Path>` and yields the underlying `View<EStr>`.
    #[inline]
    pub fn into_estr_view(self) -> View<'a, EStr> {
        View(self.0, PhantomData)
    }

    /// Returns an iterator over the views of path segments.
    #[inline]
    pub fn segments_view(self) -> SplitView<'a> {
        let absolute = self.is_absolute();
        let mut path = self.into_bytes();
        let empty = path.is_empty();

        if absolute {
            // SAFETY: Skipping "/" is fine.
            path = unsafe { path.get_unchecked_mut(1..) };
        }
        // SAFETY: The validation is done.
        let path = unsafe { View::<EStr>::new(path) };

        let mut split = path.split_view('/');
        split.finished = empty;
        split
    }
}
