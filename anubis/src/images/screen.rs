//! The seam an application looks at an uploaded image through before it is
//! stored.

use std::fmt;
use std::pin::Pin;

use super::DecodedImage;

/// What an [`ImageScreen`] decided about one image.
///
/// A refusal carries no reason on purpose. Whatever the screen saw is the
/// application's to log, and nothing the framework records or answers can
/// then repeat it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[must_use]
pub enum Verdict {
    /// The image may be stored.
    Accept,
    /// The image must not be stored, and the upload is refused.
    Refuse,
}

/// What an [`ImageScreen`] call returns.
pub type ScreenFuture<'a> = Pin<Box<dyn Future<Output = Result<Verdict, ScreenError>> + Send + 'a>>;

/// Looks at a decoded image and says whether it may be stored.
///
/// The framework calls this after an upload is decoded and held to its limits
/// and before anything is written, once per upload. An application implements
/// it to run a classifier, compare a perceptual hash against a list, or ask a
/// service, and hands it to [`crate::auth::Options::image_screen`]. A refusal
/// and an error both refuse the upload, so an implementation never needs to
/// guess in the image's favor: when it cannot tell, it returns a
/// [`ScreenError`].
///
/// The future may borrow the image. Inference is CPU-bound, so an
/// implementation that runs a model in process should copy what it needs out
/// first ([`DecodedImage::to_rgba8`] returns an owned buffer) and run the model
/// under `tokio::task::spawn_blocking`, rather than holding a runtime thread
/// for the length of the inference.
///
/// Boxed rather than an `async fn`, because a trait with one is not
/// dyn-compatible and this is only useful behind `dyn`.
pub trait ImageScreen: fmt::Debug + Send + Sync + 'static {
    /// Decides whether `image` may be stored.
    ///
    /// # Errors
    /// A [`ScreenError`] when the screen could not decide, which refuses the
    /// upload exactly as [`Verdict::Refuse`] does.
    fn screen<'a>(&'a self, image: &'a DecodedImage) -> ScreenFuture<'a>;
}

/// A shared screen is a screen, so one classifier loaded at boot can serve the
/// avatar upload and an application's own uploads from a single `Arc`.
impl<Screen: ImageScreen + ?Sized> ImageScreen for std::sync::Arc<Screen> {
    fn screen<'a>(&'a self, image: &'a DecodedImage) -> ScreenFuture<'a> {
        (**self).screen(image)
    }
}

/// Why an [`ImageScreen`] could not decide.
///
/// The message is logged on the server at `ERROR`, which the framework's error
/// reporting turns into an issue, and is never shown to the person uploading.
/// It must describe the failure (a model that would not load, a service that
/// timed out) and never the image: no pixels, no scores, no file contents.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScreenError {
    message: String,
}

impl ScreenError {
    /// A failure described by `message`, for the server's log.
    #[must_use]
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }

    /// The description the screen gave.
    #[must_use]
    pub fn message(&self) -> &str {
        &self.message
    }
}

impl fmt::Display for ScreenError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for ScreenError {}
