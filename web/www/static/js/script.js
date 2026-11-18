(function(){
	const main = document.querySelector('.main');
	
	if (!main || !rows) return;

	// --- Diff 렌더링 함수 ---
	function renderDiff(diffArray) {
		if (!diffArray || !Array.isArray(diffArray)) return "내용이 없습니다.";
		
		return diffArray.map(part => {
			const span = document.createElement('span');
			span.textContent = part.value;
			if (part.added) span.className = 'diff-added';
			if (part.removed) span.className = 'diff-removed';
			return span.outerHTML;
		}).join('');
	}

	// --- 게시글 목록 렌더링 ---
	function renderList() {
		main.innerHTML = ''; // 초기화

		if (rows.length === 0) {
			main.innerHTML = '<div class="item-content">등록된 게시글이 없습니다.</div>';
			return;
		}

		rows.forEach(item => {
			const card = document.createElement('div');
			card.className = 'item-content';

			const statusText = item.type === '0' ? 'Draft' : (item.type === '1' ? 'Premium' : 'Discount');
			const statusClass = item.type === '0' ? 'status-draft' : 'status-premium';

			let contentHtml = '';
			if (item.data && item.data.diff) {
				// 분석 완료된 경우 Diff 표시
				contentHtml = renderDiff(item.data.diff);
			} else if (item.data && item.data.body) {
				// 본문만 있는 경우 본문 표시
				contentHtml = item.data.body;
			} else {
				contentHtml = "분석 대기 중입니다...";
			}

			card.innerHTML = `
				<div class="status-badge ${statusClass}">${statusText}</div>
				<div style="font-size: 0.8em; color: #999; margin-bottom: 8px;">ID: ${item.id}</div>
				<div class="body-text">${contentHtml}</div>
				<div style="margin-top: 12px; font-size: 0.85em;">
					<a href="${item.data.link}" target="_blank" style="color: #235bf5;">원본 링크 보기</a>
				</div>
			`;
			main.appendChild(card);
		});
	}

	// 실행
	renderList();

	// 제출 버튼 이벤트 (예시)
	const submitBtn = document.querySelector('.verify');
	if (submitBtn) {
		submitBtn.addEventListener('click', async () => {
			const link = prompt("분석할 AI 공유 링크를 입력하세요:");
			if (link && link.startsWith('http')) {
				try {
					const res = await fetch('/', {
						method: 'POST',
						body: JSON.stringify({ link: link }),
						headers: { 'Content-Type': 'application/json' }
					});
					const result = await res.json();
					if (result.success) {
						alert("접수되었습니다. 잠시 후 분석이 완료됩니다.");
						location.reload();
					}
				} catch (e) {
					alert("오류가 발생했습니다.");
				}
			}
		});
	}
})();
