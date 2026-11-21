use std::sync::Arc;

use crate::error::AppError;

/// google/embeddinggemma-2 출력 차원 (서버 계약과 동일, MRL 절삭 없이 원본 768)
pub const EMBEDDING_DIM: usize = 768;

/// 텍스트를 벡터로 바꾸는 추상화. 파이프라인은 이 trait만 안다.
pub trait Embedder: Send + Sync {
    fn embed(&self, text: &str) -> Result<Vec<f32>, AppError>;

    /// `embed`가 이 문자열에 쓸 토큰 수 (특수 토큰 포함). 모델을 돌리기 전에 입력이 예산을 넘는지 확인하는 데 쓴다.
    /// 토큰을 셀 수 없는 임베더(테스트용 가짜 등)는 None을 돌려주고, 그러면 사전 검사를 건너뛴다.
    fn token_count(&self, _text: &str) -> Result<Option<usize>, AppError> {
        Ok(None)
    }

    /// 모델 타워의 상주 상태 (진단·화면용). 타워 개념이 없는 임베더는 None.
    fn residency(&self) -> Option<crate::residency::ResidencyReport> {
        None
    }
}

impl<T: Embedder + ?Sized> Embedder for Arc<T> {
    fn embed(&self, text: &str) -> Result<Vec<f32>, AppError> {
        (**self).embed(text)
    }

    fn residency(&self) -> Option<crate::residency::ResidencyReport> {
        (**self).residency()
    }

    fn token_count(&self, text: &str) -> Result<Option<usize>, AppError> {
        (**self).token_count(text)
    }
}

/// 서버 계약 검사: 768차원, 유한한 값, norm ≈ 1
pub fn check_contract(v: &[f32]) -> Result<(), AppError> {
    if v.len() != EMBEDDING_DIM {
        return Err(AppError::Embed(format!(
            "expected {} dims, got {}",
            EMBEDDING_DIM,
            v.len()
        )));
    }
    if v.iter().any(|x| !x.is_finite()) {
        return Err(AppError::Embed("vector contains non-finite values".into()));
    }
    let norm = premium_discount_cpu_kernels::dot_f32_simd(v, v).sqrt();
    if (norm - 1.0).abs() > 0.01 {
        return Err(AppError::Embed(format!("vector is not L2-normalized (norm={norm})")));
    }
    Ok(())
}

/// L2 정규화. 임베더 출력은 정규화되어 있다고 가정하지 않으므로, 계약 검사 전에 모든 벡터에 적용한다.
/// 유한하지 않은 값과 0벡터는 정규화할 수 없으므로 거부한다.
pub fn l2_normalize(mut v: Vec<f32>) -> Result<Vec<f32>, AppError> {
    if v.iter().any(|x| !x.is_finite()) {
        return Err(AppError::Embed("vector contains non-finite values".into()));
    }
    let norm = premium_discount_cpu_kernels::dot_f32_simd(&v, &v).sqrt();
    if norm == 0.0 {
        return Err(AppError::Embed("zero vector cannot be normalized".into()));
    }
    for x in v.iter_mut() {
        *x /= norm;
    }
    Ok(v)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn contract_accepts_unit_vector() {
        let v = vec![1.0 / (EMBEDDING_DIM as f32).sqrt(); EMBEDDING_DIM];
        assert!(check_contract(&v).is_ok());
    }

    #[test]
    fn contract_rejects_wrong_dim_and_unnormalized() {
        assert!(check_contract(&vec![0.1; 10]).is_err());
        assert!(check_contract(&vec![1.0; EMBEDDING_DIM]).is_err());
    }

    #[test]
    fn contract_rejects_zero_vector() {
        assert!(check_contract(&vec![0.0; EMBEDDING_DIM]).is_err());
    }

    #[test]
    fn normalize_scales_to_unit_norm_and_keeps_direction() {
        let v = l2_normalize(vec![3.0, 4.0]).unwrap();
        assert!((v[0] - 0.6).abs() < 1e-6 && (v[1] - 0.8).abs() < 1e-6);
    }

    #[test]
    fn normalized_full_size_vector_passes_contract() {
        let v = l2_normalize(vec![2.0; EMBEDDING_DIM]).unwrap();
        assert!(check_contract(&v).is_ok());
    }

    #[test]
    fn normalize_rejects_zero_and_non_finite() {
        assert!(l2_normalize(vec![0.0; EMBEDDING_DIM]).is_err());
        assert!(l2_normalize(vec![f32::NAN; EMBEDDING_DIM]).is_err());
        assert!(l2_normalize(vec![f32::INFINITY, 1.0]).is_err());
    }
}
