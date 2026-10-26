//! ONNX models for plugins: `models/<name>.onnx`, run with tract (pure Rust, deterministic).
//!
//! Train anywhere (PyTorch, scikit-learn, LightGBM, ...), export to ONNX, drop the file in the
//! models folder, and call it by name from an indicator or strategy. Each model is compiled
//! once per input shape and kept until its file changes.

use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::SystemTime,
};

use anyhow::Context;
use tract_onnx::prelude::*;

/// A dense `f32` tensor, row-major.
#[derive(Clone, Debug, PartialEq)]
pub struct Tensor {
    pub shape: Vec<usize>,
    pub data: Vec<f32>,
}

type Plan = Arc<TypedRunnableModel>;
/// Compiled models by name and input shape, with their file's modification time.
type Compiled = HashMap<(String, Vec<usize>), (SystemTime, Plan)>;

/// The models folder, with compiled models cached.
#[derive(Debug)]
pub struct Models {
    dir: PathBuf,
    cache: Mutex<Compiled>,
}

impl Models {
    #[must_use]
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self {
            dir: dir.into(),
            cache: Mutex::new(HashMap::new()),
        }
    }

    /// `~/Library/Application Support/persephone/models` on macOS, and the platform
    /// equivalent elsewhere.
    #[must_use]
    pub fn default_dir() -> Option<PathBuf> {
        dirs::data_dir().map(|dir| dir.join("persephone").join("models"))
    }

    #[must_use]
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// The names of the models in the folder, sorted.
    #[must_use]
    pub fn names(&self) -> Vec<String> {
        let Ok(entries) = fs::read_dir(&self.dir) else {
            return Vec::new();
        };
        let mut names: Vec<String> = entries
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.extension().is_some_and(|ext| ext == "onnx"))
            .filter_map(|p| Some(p.file_stem()?.to_str()?.to_owned()))
            .collect();
        names.sort();
        names
    }

    /// Runs model `name` on `input`, returning every output as `f32` (integer outputs, such as
    /// a classifier's label, are converted).
    ///
    /// # Errors
    ///
    /// Returns a message if there's no such model, it isn't valid ONNX, it doesn't take this
    /// input, or inference fails.
    pub fn run(&self, name: &str, input: &Tensor) -> Result<Vec<Tensor>, String> {
        check_name(name)?;
        let expected: usize = input.shape.iter().product();
        if input.shape.is_empty() || expected != input.data.len() {
            return Err(format!(
                "the input has {} values but its shape {:?} needs {expected}",
                input.data.len(),
                input.shape
            ));
        }
        let plan = self.plan(name, &input.shape)?;
        let tensor = tract_ndarray::ArrayD::from_shape_vec(input.shape.clone(), input.data.clone())
            .map_err(|e| e.to_string())?;
        let outputs = plan
            .run(tvec!(tract_onnx::prelude::Tensor::from(tensor).into()))
            .map_err(|e| format!("model `{name}` failed: {e}"))?;
        outputs.iter().map(to_f32).collect()
    }

    fn plan(&self, name: &str, shape: &[usize]) -> Result<Plan, String> {
        let path = self.dir.join(format!("{name}.onnx"));
        let modified = fs::metadata(&path)
            .and_then(|m| m.modified())
            .map_err(|_| format!("there's no model `{name}` (looked for {})", path.display()))?;
        let key = (name.to_owned(), shape.to_vec());
        let mut cache = self
            .cache
            .lock()
            .map_err(|_| "the model cache is poisoned")?;
        if let Some((when, plan)) = cache.get(&key)
            && *when == modified
        {
            return Ok(Arc::clone(plan));
        }
        let plan = compile(&path, shape).map_err(|e| format!("model `{name}`: {e:#}"))?;
        cache.insert(key, (modified, Arc::clone(&plan)));
        Ok(plan)
    }
}

fn compile(path: &Path, shape: &[usize]) -> anyhow::Result<Plan> {
    let model = tract_onnx::onnx()
        .model_for_path(path)
        .context("not a model tract can read")?
        .with_input_fact(0, f32::fact(shape).into())
        .context("it doesn't take one f32 input of this shape")?
        .into_optimized()?
        .into_runnable()?;
    Ok(model)
}

fn to_f32(value: &TValue) -> Result<Tensor, String> {
    let converted = value
        .cast_to::<f32>()
        .map_err(|e| format!("an output isn't numeric: {e}"))?;
    let view = converted
        .to_plain_array_view::<f32>()
        .map_err(|e| e.to_string())?;
    Ok(Tensor {
        shape: view.shape().to_vec(),
        data: view.iter().copied().collect(),
    })
}

/// Model names are file names: letters, digits, `-` and `_`.
fn check_name(name: &str) -> Result<(), String> {
    let ok = !name.is_empty()
        && name.len() <= 64
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
    if ok {
        Ok(())
    } else {
        Err(format!(
            "`{name}` isn't a model name (letters, digits, `-`, `_`)"
        ))
    }
}

/// An ONNX logistic regression, `sigmoid(features · weights + bias)`: input `features` of
/// shape `[batch, weights.len()]`, output `probability` of shape `[batch, 1]`. A model to try
/// the `ml` capability with, no training tools needed.
///
/// # Panics
///
/// Never in practice: encoding a model to memory can't fail.
#[must_use]
pub fn logistic_regression(weights: &[f32], bias: f32) -> Vec<u8> {
    use prost::Message;
    use tract_onnx::pb::{
        GraphProto, ModelProto, NodeProto, OperatorSetIdProto, TensorProto, TensorShapeProto,
        TypeProto, ValueInfoProto,
        tensor_shape_proto::{Dimension, dimension},
        type_proto,
    };

    const FLOAT: i32 = 1;
    let dim = |value: dimension::Value| Dimension {
        value: Some(value),
        ..Dimension::default()
    };
    let value_info = |name: &str, dims: Vec<Dimension>| ValueInfoProto {
        name: name.into(),
        r#type: Some(TypeProto {
            value: Some(type_proto::Value::TensorType(type_proto::Tensor {
                elem_type: FLOAT,
                shape: Some(TensorShapeProto { dim: dims }),
            })),
            ..TypeProto::default()
        }),
        ..ValueInfoProto::default()
    };
    let tensor = |name: &str, dims: Vec<i64>, values: Vec<f32>| TensorProto {
        name: name.into(),
        dims,
        data_type: FLOAT,
        float_data: values,
        ..TensorProto::default()
    };
    let node = |op: &str, inputs: &[&str], output: &str| NodeProto {
        op_type: op.into(),
        input: inputs.iter().map(|&i| i.into()).collect(),
        output: vec![output.into()],
        ..NodeProto::default()
    };
    let batch = || dim(dimension::Value::DimParam("batch".into()));

    let model = ModelProto {
        ir_version: 8,
        opset_import: vec![OperatorSetIdProto {
            domain: String::new(),
            version: 13,
        }],
        producer_name: "persephone".into(),
        graph: Some(GraphProto {
            name: "logistic_regression".into(),
            node: vec![
                node("MatMul", &["features", "weights"], "dot"),
                node("Add", &["dot", "bias"], "logit"),
                node("Sigmoid", &["logit"], "probability"),
            ],
            initializer: vec![
                tensor("weights", vec![weights.len() as i64, 1], weights.to_vec()),
                tensor("bias", vec![1], vec![bias]),
            ],
            input: vec![value_info(
                "features",
                vec![
                    batch(),
                    dim(dimension::Value::DimValue(weights.len() as i64)),
                ],
            )],
            output: vec![value_info(
                "probability",
                vec![batch(), dim(dimension::Value::DimValue(1))],
            )],
            ..GraphProto::default()
        }),
        ..ModelProto::default()
    };
    model.encode_to_vec()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn folder(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("persephone-models-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn sigmoid(x: f32) -> f32 {
        1.0 / (1.0 + (-x).exp())
    }

    #[test]
    fn runs_a_logistic_regression() {
        let dir = folder("logistic");
        fs::write(
            dir.join("trend.onnx"),
            logistic_regression(&[2.0, -1.0, 0.5], 0.25),
        )
        .unwrap();
        let models = Models::new(&dir);
        assert_eq!(models.names(), ["trend"]);

        let input = Tensor {
            shape: vec![2, 3],
            data: vec![1.0, 2.0, 3.0, 0.0, 0.0, 0.0],
        };
        let out = models.run("trend", &input).unwrap();
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].shape, vec![2, 1]);
        let expected = [sigmoid(2.0 - 2.0 + 1.5 + 0.25), sigmoid(0.25)];
        for (got, want) in out[0].data.iter().zip(expected) {
            assert!((got - want).abs() < 1e-6, "{got} vs {want}");
        }
        // Another shape compiles its own plan; the same shape reuses one.
        let one = Tensor {
            shape: vec![1, 3],
            data: vec![0.0; 3],
        };
        assert!((models.run("trend", &one).unwrap()[0].data[0] - sigmoid(0.25)).abs() < 1e-6);
        assert_eq!(models.cache.lock().unwrap().len(), 2);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn explains_what_went_wrong() {
        let dir = folder("errors");
        fs::write(
            dir.join("trend.onnx"),
            logistic_regression(&[1.0, 1.0], 0.0),
        )
        .unwrap();
        fs::write(dir.join("junk.onnx"), b"not onnx").unwrap();
        let models = Models::new(&dir);
        let ok = Tensor {
            shape: vec![1, 2],
            data: vec![0.0, 0.0],
        };

        let missing = models.run("nope", &ok).unwrap_err();
        assert!(missing.contains("no model `nope`"), "{missing}");
        assert!(
            models
                .run("../etc/passwd", &ok)
                .unwrap_err()
                .contains("isn't a model name")
        );
        assert!(models.run("junk", &ok).unwrap_err().contains("junk"));
        let wrong = Tensor {
            shape: vec![1, 3],
            data: vec![0.0; 3],
        };
        assert!(
            models.run("trend", &wrong).is_err(),
            "three features into a two-feature model"
        );
        let ragged = Tensor {
            shape: vec![1, 2],
            data: vec![0.0; 3],
        };
        assert!(
            models
                .run("trend", &ragged)
                .unwrap_err()
                .contains("needs 2")
        );
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_changed_file_is_recompiled() {
        let dir = folder("reload");
        let path = dir.join("m.onnx");
        let models = Models::new(&dir);
        let input = Tensor {
            shape: vec![1, 1],
            data: vec![1.0],
        };
        fs::write(&path, logistic_regression(&[0.0], 0.0)).unwrap();
        assert!((models.run("m", &input).unwrap()[0].data[0] - 0.5).abs() < 1e-6);

        fs::write(&path, logistic_regression(&[0.0], 10.0)).unwrap();
        // Make sure the modification time moves even on coarse-grained file systems.
        let later = SystemTime::now() + std::time::Duration::from_secs(5);
        fs::File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_modified(later)
            .unwrap();
        assert!(models.run("m", &input).unwrap()[0].data[0] > 0.99);
        fs::remove_dir_all(dir).unwrap();
    }
}
