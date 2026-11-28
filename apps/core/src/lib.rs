//! 평가 시스템 클라이언트 로직.
//! - 욕설 필터 -> 임베딩 -> 서버 저장 (submit)
//! - 질문 임베딩 -> 서버 벡터 검색 (search)
//! 임베딩·양자화·GGUF 캐시 코어는 Tier 1(`lv-core`)을 단일 원천으로 사용하고,
//! `embeddinggemma.rs`가 앱 상주 정책(`residency.rs`)과 대칭 계약(`SymmetricGemmaEmbedder`)만 연결한다.

pub mod analytic;
pub mod api;
pub mod commands;
pub mod embedding;
pub mod embeddinggemma;
pub use lv_core::gemma_audio as embeddinggemma_audio;
pub use lv_core::gemma_embed;
pub use lv_core::gemma_vision;
pub use lv_core::qcache;
pub mod embeddinggemma_tokens {
    pub use lv_core::gemma_embed::GemmaTokenTable as TokenTable;
}
pub mod quant {
    pub use lv_core::quant::*;

    /// 지시서 4계층 텍스트 백본 피라미드 양자화 레시피 (`lv_core::quant::QuantRecipe::tiered_text_pyramid`).
    pub fn pyramid_v1() -> QuantRecipe {
        QuantRecipe::tiered_text_pyramid()
    }
}
pub mod error;
pub mod filter;
pub mod items_draft;
pub mod laya;
pub mod local;
pub mod model_fetch;
pub mod nav_live;
pub mod outbox;
pub mod pipeline;
pub mod qwen35;
pub mod queue;
pub mod residency;
pub mod session;
pub mod utils;
pub mod votes;

pub use api::{
    EvalBackend, HttpBackend, Match, MatchVerdict, SaveRequest, SaveResponse, SearchRequest, SearchResponse, Session,
    new_request_id, text_hash, StatusResponse, VerdictDto, VerdictsResponse, VoteRequest, VoteResponse, ANONYMOUS_HASH,
    ANONYMOUS_TOKEN, VERDICT_IDS_MAX,
};
pub use commands::{
    AppCore, AppInfo, AppStatus, CommandError, platform, FlushSummary, LocalMatchDto, LoginStartResult, LoginStatus, SearchArgs, SearchLocalArgs, SessionArgs, SessionInfo,
    SubmitArgs, SubmitResult, SyncArgs,
};
pub use embedding::{check_contract, Embedder, EMBEDDING_DIM};
pub use error::AppError;
pub use filter::{FilterSetting, LocalFilter, ProfanityFilter, SwitchFilter, Verdict};
pub use embeddinggemma::{
    weights_file, EmbedMode, EmbeddingGemma, LoadOptions, Part, SymmetricGemmaEmbedder, TEXT_WEIGHTS_FILE,
    TOKENIZER_FILE, WEIGHTS_FILE,
};
pub use residency::{Modalities, ResidencyReport, TowerState};
pub use quant::{Bits, Kernel, ProjGroup, QuantEncoder, QuantRecipe, QuantSetting, TowerQuantPlan};
pub use model_fetch::{DownloadArgs, FetchProgress, FetchState, FilterInfo, Installed, ModelFetcher, ModelStatus, Variant};
pub use pipeline::{
    merge_matches, Evaluator, MatchSource, MergedMatch, MergedSearch, SearchOptions, SubmitInput, SubmitOutcome, MAX_TEXT_BYTES,
    TOP_K_MAX, TOP_K_MIN,
};
pub use local::{LocalDb, LocalMatch, LocalRecord, LocalState};
pub use outbox::{ImportReport, Outbox};
pub use session::{SavedSession, SessionStore};
pub use queue::{FlushReport, OfflineQueue, ParentRef, SyncReport, SyncedItem};
pub use votes::{VoteFlushReport, VoteKind, VoteRecord, VoteState, VoteStore, VERDICT_TTL_MS};
