-- 지갑 서명 한 번만 쓰기 (fix-wallet-replay). 쓰는 곳은 www/src/auth.rs 의 verify_wallet_request_once (지갑 서명을 받는 모든 경로:
-- /nav/trials, /analytic/sites, /analytic/events 의 주인 읽기 · 지우기).
-- 지갑 서명 글(contracts/wallet_v1.json)에는 경로만 있고 쿼리가 없어, 잡은 서명 하나를 5분 안에 다른 쿼리로 다시 보낼 수 있었다
-- (DELETE /nav/trials?app=X 를 DELETE /nav/trials 로 다시 보내면 콜렉션 전부가 지워진다). 계약은 그대로 두고 서버가 받은 서명을 기억해
-- 다시 쓰면 막는다(401).
-- nonce: 서명한 글(주소 · 시각 · 메서드 · 경로 · 본문 해시)의 SHA-256 hex. 서명 바이트가 아니라 글이라 같은 글의 다른 서명 꼴
--   (v 27 과 0, 높은 s, 16진 대소문자)도 같은 키다.
-- expires_at: 서명 시각 + 5분 + 60초 (ms). 시각 검사는 서명 시각 + 5분 뒤로 이 서명을 받지 않고, 60초는 isolate 사이 시계 차이를 위한
--   여유다(받는지는 시각 검사만 정하고 이 값은 지우기에만 쓴다). 지갑 요청마다 지난 행을 10개까지 지운다(요청 하나가 행 하나를 넣으므로
--   표는 최근 약 11분의 지갑 요청 수로 묶인다).
-- 적용 순서: 0010 -> (그 뒤에) www 를 배포한다. 표가 없으면 지갑 요청이 D1 오류로 500 이 된다.

CREATE TABLE IF NOT EXISTS wallet_nonces (
	"nonce" TEXT PRIMARY KEY,
	"expires_at" INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_wallet_nonces_expiry ON wallet_nonces ("expires_at");
