"""laya-multilingual 참조 픽스처를 만든다 (진짜 laya 패키지 + torch로 실행).

Rust 이식(app/src/laya.rs)이 같은 입력에 같은 로짓·확률을 내는지 확인하는 데 쓴다.
판: laya 0.4.31, transformers 5.23.0, torch 2.15.0+cpu, 모델 리비전 1720e3e.

    HF_HOME=<캐시> HF_HUB_OFFLINE=1 python laya_reference.py <모델 폴더> <출력 json>
"""
import json
import sys

import torch

import laya
from laya.common import QTYPES

MODEL, OUT = sys.argv[1], sys.argv[2]
agent = laya.load(MODEL)
agent.model.eval()

# 앱의 욕설 판정 질문 (laya-multilingual/src/laya.rs의 PROFANITY_QUESTION과 같아야 한다).
# README의 권고대로 noul 대신 중립 키(A/B) 두 개짜리 choice로 묻는다.
QUESTION = {
    "type": "choice",
    "instructions": "Does `body` contain profanity, slurs, sexual insults or abusive language aimed at someone?",
    "criteria": {
        "A": "yes, it contains profanity, slurs or abusive insults",
        "B": "no, the language is polite or neutral",
    },
}
# 유형 경로를 하나 더 확인한다 (noul: qtype 2)
NOUL = {"type": "noul", "instructions": "Is `body` a complaint about a delivery?"}

TEXTS = [
    "배송이 빨라서 만족합니다. 다시 구매할게요.",
    "시발 이게 뭐야 돈 아깝다",
    "당시 발견된 문제는 바로 해결되었습니다.",
    "판매자 진짜 개새끼네 연락도 안 받음",
    "The seller was polite and the package arrived early.",
    "This is a fucking scam, you piece of shit.",
    "포장이 조금 아쉬웠지만 제품은 좋아요.",
    "병신같은 서비스 다신 안 씀",
    "Shipping took forever and nobody answered my emails.",
    "Das Paket kam beschädigt an, aber der Support war freundlich.",
    # 512 토큰이 넘는 긴 입력: laya가 CPU에서 창 어텐션 경로(_cpu_window_encoder)를 탄다
    "주문한 상품이 늦게 도착했습니다. " * 70 + "그래도 판매자는 친절했습니다.",
]

captured = []


def hook(module, args, kwargs, output):
    input_ids = kwargs.get("input_ids", args[0] if args else None)
    marker_pos = kwargs.get("marker_pos", args[2] if len(args) > 2 else None)
    qtype = kwargs.get("qtype", args[4] if len(args) > 4 else None)
    logits, act = output
    captured.append({
        "ids": input_ids[0].tolist(),
        "markers": marker_pos[0].tolist(),
        "qtype": int(qtype[0]),
        "logits": logits[0].tolist(),
        "act": torch.softmax(act[0].float(), -1).tolist(),
    })


# 인코더 출력(마지막 은닉 상태)도 일부 남긴다 (어긋날 때 어디서 어긋났는지 보려고)
enc_out = []
agent.model.encoder.register_forward_hook(lambda m, a, o: enc_out.append(o.last_hidden_state[0]))
agent.model.register_forward_hook(hook, with_kwargs=True)

cases = []
for text in TEXTS:
    for name, q in (("profanity", QUESTION), ("delivery_complaint", NOUL)):
        captured.clear()
        enc_out.clear()
        with torch.no_grad():
            res = agent.predict({"body": text}, {name: q})
        assert len(captured) == 1, "질문 하나에 forward 한 번"
        c = captured[0]
        ans = res["answers"][name]
        h = enc_out[0] if enc_out else None
        cases.append({
            "text": text,
            "question": name,
            "ids": c["ids"],
            "markers": c["markers"],
            "qtype": c["qtype"],
            "logits": c["logits"][: len(c["markers"])],
            "probs": ans.get("probs") or ans.get("probabilities"),
            "answer": ans.get("choice", ans.get("value")),
            # 인코더 출력: 0번 위치와 표지 위치의 앞 16차원 (창 경로는 훅이 없어 비어 있을 수 있다)
            "enc0": h[0, :16].tolist() if h is not None else None,
            "enc_markers": [h[m, :16].tolist() for m in c["markers"]] if h is not None else None,
        })
        print(name, len(c["ids"]), "tokens", c["logits"][: len(c["markers"])], ans.get("choice", ans.get("value")))

json.dump({"model": "convaiinnovations/laya-multilingual", "revision": "1720e3e3357cfe1e281542e223f8273b0890ca34",
           "laya": "0.4.31", "question": QUESTION, "noul": NOUL, "qtypes": QTYPES, "cases": cases},
          open(OUT, "w"), ensure_ascii=False)
print("cases", len(cases))
