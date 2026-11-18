"""Qwen3.5-0.8B 참조 픽스처를 만든다 (transformers 5.23.0, torch 2.15.0+cpu, F32).

Rust 이식(app/src/qwen35.rs)이 같은 프롬프트에 같은 그리디 토큰과 로짓을 내는지 확인하는 데 쓴다.

    HF_HOME=<캐시> HF_HUB_OFFLINE=1 python qwen35_reference.py <모델 폴더> <출력 json>
"""
import json
import sys

import torch
from transformers import AutoModelForCausalLM, AutoTokenizer

MODEL, OUT = sys.argv[1], sys.argv[2]
torch.manual_seed(0)
tok = AutoTokenizer.from_pretrained(MODEL)
model = AutoModelForCausalLM.from_pretrained(MODEL, dtype=torch.float32)
model.eval()

SYSTEM = "You read a shopping web page and write a JSON draft."
PAGES = [
    "겨울 패딩 최대 70% 할인! 오늘 자정까지 쿠폰 적용 시 추가 10% 할인됩니다. 무료 배송.",
    "Hand-stitched Italian leather briefcase, full-grain calfskin, lifetime warranty. Limited edition of 200.",
]
cases = []
for page in PAGES:
    messages = [{"role": "system", "content": SYSTEM}, {"role": "user", "content": page}]
    prompt = tok.apply_chat_template(messages, tokenize=False, add_generation_prompt=True, enable_thinking=False)
    ids = tok(prompt, add_special_tokens=False)["input_ids"]
    with torch.no_grad():
        gen = model.generate(torch.tensor([ids]), max_new_tokens=20, do_sample=False)[0].tolist()
        new = gen[len(ids):]
        logits = model(torch.tensor([gen])).logits[0]
    steps = []
    for p in range(len(ids) - 1, len(gen) - 1):
        top = torch.topk(logits[p], 10)
        steps.append({"ids": top.indices.tolist(), "values": top.values.tolist()})
    cases.append({"page": page, "prompt": prompt, "ids": ids, "generated": new, "text": tok.decode(new), "steps": steps})
    print(len(ids), "prompt tokens ->", repr(tok.decode(new)))

# 순수 텍스트(채팅 템플릿 없이) 한 건: 다음 토큰 로짓 상위 20
plain = "The capital of France is"
pids = tok(plain, add_special_tokens=False)["input_ids"]
with torch.no_grad():
    lg = model(torch.tensor([pids])).logits[0, -1]
top = torch.topk(lg, 20)
json.dump({"model": "Qwen/Qwen3.5-0.8B", "revision": "2fc06364715b967f1860aea9cf38778875588b17", "system": SYSTEM,
           "cases": cases, "plain": {"text": plain, "ids": pids, "top_ids": top.indices.tolist(), "top_values": top.values.tolist()},
           "special": {name: tok.convert_tokens_to_ids(name) for name in ["<|im_start|>", "<|im_end|>", "<|endoftext|>", "<think>", "</think>"]}},
          open(OUT, "w"), ensure_ascii=False)
print("plain top", tok.convert_ids_to_tokens(top.indices[:5].tolist()))
