//! ONNX models from Persephone's models folder, run by the host at native speed.
//!
//! Train a model anywhere (PyTorch, scikit-learn, LightGBM, ...), export it to ONNX and save
//! it as `<name>.onnx` in the models folder (`persephone-cli models dir` prints where), then:
//!
//! ```ignore
//! let p = ml::run("trend", &ml::Tensor::row(&features))?;
//! let probability = p[0].data[0];
//! ```

#[cfg(feature = "strategy")]
use crate::strategy_bindings::persephone::plugin::ml as host;

#[cfg(all(feature = "indicator", not(feature = "strategy")))]
use crate::indicator_bindings::persephone::plugin::ml as host;

/// A dense `f32` tensor, row-major.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Tensor {
    pub shape: Vec<u32>,
    pub data: Vec<f32>,
}

impl Tensor {
    /// One row of features: shape `[1, values.len()]`.
    #[must_use]
    pub fn row(values: &[f32]) -> Self {
        Self {
            shape: vec![1, values.len() as u32],
            data: values.to_vec(),
        }
    }

    /// Rows of equal length stacked: shape `[rows.len(), width]`.
    ///
    /// # Panics
    ///
    /// Panics if the rows aren't all the same length.
    #[must_use]
    pub fn rows(rows: &[Vec<f32>]) -> Self {
        let width = rows.first().map_or(0, Vec::len);
        assert!(
            rows.iter().all(|r| r.len() == width),
            "rows of different lengths"
        );
        Self {
            shape: vec![rows.len() as u32, width as u32],
            data: rows.concat(),
        }
    }
}

/// Runs `models/<name>.onnx` on `input`: one tensor per model output. Integer outputs, such as
/// a classifier's label, come back as floats.
///
/// # Errors
///
/// Returns the host's message: no such model, the input doesn't fit it, inference failed.
pub fn run(name: &str, input: &Tensor) -> Result<Vec<Tensor>, String> {
    let input = host::Tensor {
        shape: input.shape.clone(),
        data: input.data.clone(),
    };
    host::run(name, &input).map(|outputs| {
        outputs
            .into_iter()
            .map(|t| Tensor {
                shape: t.shape,
                data: t.data,
            })
            .collect()
    })
}
