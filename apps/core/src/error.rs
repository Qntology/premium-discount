#[derive(Debug, thiserror::Error)]
pub enum AppError {
    #[error("invalid input: {0}")]
    InvalidInput(String),
    #[error("embedding failed: {0}")]
    Embed(String),
    #[error("filter failed: {0}")]
    Filter(String),
    /// 연결 실패·시간 초과: 네트워크가 없다는 뜻. 큐에 두고 나중에 다시 보낸다.
    #[error("offline: {0}")]
    Offline(String),
    #[error("request failed: {0}")]
    Http(#[from] reqwest::Error),
    /// 2xx 응답인데 본문이 JSON 계약과 맞지 않는다 (끝까지 닿았는데 해석할 수 없음).
    /// 서버가 요청을 받았을 수 있으므로 바로 버리지 않고 같은 request_id로 다시 보낸다 (재시도 한도는 있다).
    #[error("unreadable response: {0}")]
    BadResponse(String),
    #[error("server returned {status}: {message}")]
    Server { status: u16, message: String },
    /// 서버가 영구적으로 거부한 저장 (큐에서 버림)
    #[error("rejected: {0}")]
    Rejected(String),
    /// 대기열이 상한에 도달했다. 새 저장은 받지 않는다 (큐의 항목 수)
    #[error("queue is full ({0} pending items)")]
    QueueFull(usize),
    /// 로컬 저장소 읽기·쓰기 실패 또는 저장 내용이 깨짐
    #[error("storage failed: {0}")]
    Storage(String),
}
