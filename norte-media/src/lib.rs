#![forbid(unsafe_code)]
#![doc = "Optional media preprocessing for `Norte`: resize and re-encode images before they reach a model. Provider-agnostic and configured by the host."]

mod image;

pub use image::{ImageOutput, ImagePreprocessor};
