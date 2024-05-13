use super::parse_device;
use crate::common::{hf_hub_get, OptionToResult, ResultExt};
use anyhow::Result;
use candle_core::{DType, Device, Tensor};
use candle_nn::VarBuilder;
use candle_transformers::models::bert::{BertModel, Config as BertConfig};
use once_cell::sync::OnceCell;
use std::sync::Arc;
use tokenizers::{PaddingParams, Tokenizer};
use tokio::sync::Mutex;

pub const E5_MODEL_REPO: &str = "intfloat/e5-small-v2";

static E5_MODEL: OnceCell<Arc<Mutex<E5Model>>> = OnceCell::new();

pub struct E5Model {
    pub model: BertModel,
    pub tokenizer: Tokenizer,
    pub normalize_embeddings: Option<bool>,
    pub device: Device,
}

impl E5Model {
    pub fn init(e5_model_repo: &str, device_type: &str) -> Result<()> {
        let e5_model = E5Model::load(e5_model_repo, device_type)?;
        let _ = E5_MODEL.set(Arc::new(Mutex::new(e5_model))).is_ok();
        Ok(())
    }

    pub async fn embeddings(input: Vec<String>) -> Result<Vec<Vec<f32>>> {
        let model = E5_MODEL.get().ok_or_err("E5_MODEL")?.lock().await;
        let embeddings_data = model.forward(input)?;
        Ok(embeddings_data)
    }

    pub fn load(e5_model_repo: &str, device_type: &str) -> Result<E5Model> {
        let weights = hf_hub_get(e5_model_repo, "model.safetensors", None, None)?;
        let tokenizer = hf_hub_get(e5_model_repo, "tokenizer.json", None, None)?;
        let candle_config = hf_hub_get(e5_model_repo, "config.json", None, None)?;
        let candle_config: BertConfig = serde_json::from_slice(&candle_config)?;

        let device = parse_device(device_type)?;
        let mut tokenizer = Tokenizer::from_bytes(&tokenizer).map_anyhow_err()?;

        if let Some(pp) = tokenizer.get_padding_mut() {
            pp.strategy = tokenizers::PaddingStrategy::BatchLongest
        } else {
            let pp = PaddingParams {
                strategy: tokenizers::PaddingStrategy::BatchLongest,
                ..Default::default()
            };
            tokenizer.with_padding(Some(pp));
        }

        let vb = VarBuilder::from_buffered_safetensors(weights, DType::F32, &device)?;
        let model = BertModel::load(vb, &candle_config)?;
        Ok(E5Model {
            model,
            tokenizer,
            normalize_embeddings: Some(true),
            device,
        })
    }

    pub fn forward(&self, input: Vec<String>) -> Result<Vec<Vec<f32>>> {
        let device = &self.device;
        let tokens = self
            .tokenizer
            .encode_batch(input.clone(), true)
            .map_anyhow_err()?;

        let token_ids: Vec<Tensor> = tokens
            .iter()
            .map(|tokens| {
                let tokens = tokens.get_ids().to_vec();
                Tensor::new(tokens.as_slice(), device)
            })
            .collect::<std::result::Result<Vec<_>, _>>()?;

        let token_ids = Tensor::stack(&token_ids, 0)?;
        let token_type_ids = token_ids.zeros_like()?;

        let embeddings = self.model.forward(&token_ids, &token_type_ids)?;
        let (_n_sentence, n_tokens, _hidden_size) = embeddings.dims3()?;
        let embeddings = (embeddings.sum(1)? / (n_tokens as f64))?;
        let embeddings = if let Some(true) = self.normalize_embeddings {
            embeddings.broadcast_div(&embeddings.sqr()?.sum_keepdim(1)?.sqrt()?)?
        } else {
            embeddings
        };
        let embeddings_data: Vec<Vec<f32>> = embeddings.to_vec2()?;
        Ok(embeddings_data)
    }
}