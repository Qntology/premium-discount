export function renderHtml(cookies, rows) {
	return `
		<!DOCTYPE html>
		<html>
		<head>
			<meta charset="utf-8">
			<meta name="viewport" content="width=device-width, initial-scale=1.0" />
			
			<meta property="og:title" content="premium.discount">
			<meta property="og:url" content="https://www.premium.discount/">
			<meta property="og:image" content="https://www.premium.discount/static/img/common/favicon.ico">
			<meta property="og:description" content="premium.discount">

			<meta name="popup-link-verification" content="6757d390597f497229ca4087048d3c32f58b67c85432e270d0d340e911e38263" />
			<meta name="privacy" content="/privacy/" />
			<meta name="terms" content="/terms/" />

			<link rel="stylesheet" href="/static/fonts/fontawesome/css/all.min.css">
			<link rel="stylesheet" href="/static/fonts/xeicon/xeicon.min.css">

			<link rel="stylesheet" href="/static/css/style.css">
			<link rel="stylesheet" href="/static/css/main.css">
			<link rel="stylesheet" href="/static/css/host.css">

			<style>
				body[mobile] #footer{display: none; background-color: #f6f6f6;}

				body{overflow-y: scroll !important;}
				.article.full{}
				.title_style01 a{display: inline; margin-left: 5px; text-decoration: underline;}

				body[mobile] .title_style01 .title span,
				body[mobile] .title_style01 .desc span{color:#000;}

				body[mobile][open] .title_style01 .title span,
				body[mobile][open] .title_style01 .desc span{color:#fff;}
				body[mobile][open] .title_style01 .title span,
				body[mobile][open] .title_style01 .desc span{text-shadow: 0 0 5px black;}

				.article{opacity: 0.2; transition-duration: 0.5s;}		
				.article.on{opacity: 1;}
				.article.pricing .tc{text-align: center;}
				.article.pricing .table{display: inline-block;}
				.article.pricing .table th,
				.article.pricing .table tc{padding: 1em;}

				.form_nav{text-align: center;}
				.form_nav a{display: inline-block; height: 5em; line-height: 5em; vertical-align: top;}
				.form_nav a+a{margin-left: 1em;}
				.form_nav a img{vertical-align: middle;}
				body[mobile] .wrap>*{position: relative; z-index: 110;}

				body[section] popup.link a{pointer-events: none !important;}

				body[mobile] popup.link tip{display: none;}

				
				body[mobile] popup.link a{opacity: 0;}
				body[mobile][end] popup.link a{opacity: 1;}
				body[mobile][end] popup.link layer iframe{opacity: 1;}
			
				body[mobile] #header .area,
				body[mobile] #headline{opacity: 1; transition-duration: 0.5s;}

				body[mobile][open] #header .area,
				body[mobile][open] #headline{opacity: 0;}

				body[mobile][open] #header{border-color: transparent;}


				.title_style01{display: block; padding: 8em 0; text-align: center;}
				.title_style01 .title{margin-bottom:15px;}
				.title_style01 .title span{font-size:34px; font-weight:900; word-break: keep-all; line-height: 1.5; letter-spacing: 1px;}
				.title_style01 .desc span{display: block; font-size:20px; font-weight:900; word-break: keep-all; line-height: 1.5;}
				.title_style01 a{display: none;}

				#headline{position: relative; margin: 4em 0 7em;text-align: center; z-index: 0;}
				#headline h2{margin-top: 0.1em; font-size: 3.3em; font-weight: 200; line-height: 1.2; word-break: keep-all;}
				body[mobile] #headline h2{font-size: 2.75em}

				#headline strong{display: inline-block; position: relative; font-size: 1em; font-weight: 900;}
				#headline span{margin-top: 25px; font-size: 20px; line-height: 1.3; word-break: keep-all;}

				#headline a{font-size: 1em; font-weight: 700; color:#235bf5;}
				#headline a.verify{display: inline-block; margin-top: 40px; padding: 18px 50px 21px; border-radius: 7px; text-decoration: inherit; font-size:18px; font-weight: 400; background-color: #235bf5; color: #fff;}

				.notification{position: absolute; left: 1em; top: 1em; right: 1em; padding: 2.8em 1em 1em; border-radius: 1em; z-index: 1; background: rgb(255 255 255 / 95%); overflow: hidden;}
				
				.notification .title{margin-bottom: 4px;}
				.notification .title span{font-size: 13px; font-weight: bold;}
				.notification .desc span{font-size: 10px; font-weight: bold;}


				.notification .title:before{content: "\f0f3"; font-family: "Font Awesome 6 Free"; font-weight: 600; position: absolute; left: 1em; top: 1em;}
				.notification .title span:before{content: "popup.link"; position: absolute; left: 3.5em; top: 1.5em; font-size: 10px; font-weight: bold;}
				.notification .title span:after{content: "now"; position: absolute; right: 1.5em; top: 1em; font-size: 10px; font-weight: bold;}

				@media (max-width: 999px) {
					.mobile[style]{display: none;}
				}
			</style>

			<title>premium.discount</title>

			<style>
				body { font-family: sans-serif; text-align: center; margin: 0; padding: 1rem; background-color: #f4f4f9; }
				.container { max-width: 640px; margin: 0 auto; background: #fff; padding: 2rem; border-radius: 8px; box-shadow: 0 4px 6px rgba(0,0,0,0.1); }
				h1 { color: #333; }

				.premium{background: #00a83e;}
				.discount{background: #ff3a33}

				.diff-added { background-color: #e6ffec; color: #24292f; text-decoration: none; border-bottom: 2px solid #2da44e; }
				.diff-removed { background-color: #ffebe9; color: #cf222e; text-decoration: line-through; }
				.item-content { text-align: left; padding: 1em; border: 1px solid #ddd; border-radius: 8px; margin: 1em 0; background: #fff; }
				.status-badge { display: inline-block; padding: 2px 8px; border-radius: 12px; font-size: 12px; font-weight: bold; margin-bottom: 8px; }
				.status-draft { background: #eee; color: #666; }
				.status-premium { background: #e6ffec; color: #2da44e; }

				#header h1 a{display:block; width: 13px;}
				#header h1 a span{display:block; height: 13px;}

				
			</style>
		</head>
		<body>
			<div class="wrap">
				<div id="header">
					<div class="area">
						<h1>
							<a href="/" alt="premium.discount">
								<span class="premium"></span>
								<span class="discount"></span>
							</a>
						</h1>

						<label for="nav"></label>
						<input id="nav" type="checkbox">
						<nav></nav>
					</div>

					<div id="headline">
						<div class="area">
							<h2><strong>PREMIUM</strong> / <strong>DISCOUNT</strong></h2>
							<span><a>#REVIEW</a></span>
							<a class="verify">SUBMIT</a>
						</div>
					</div>
				</div>

				<div class="main">
<div class="section graph-entry" style="padding: 2em; background: #fff; margin: 1em auto; max-width: 600px; border-radius: 10px; box-shadow: 0 4px 10px rgba(0,0,0,0.05);">
	<h2 style="font-size: 1.5em; margin-bottom: 0.5em;">평가 그래프</h2>
	<p style="color: #666; font-size: 0.9em; margin-bottom: 1.5em;">도메인을 입력하면 평가, 도메인, 작성자의 관계를 그래프로 볼 수 있습니다. 작성자는 표시용 키로만 나옵니다.</p>
	<div style="display: flex; gap: 10px;">
		<input type="text" id="graphCc" placeholder="example.com" style="flex: 1; padding: 12px; border: 1px solid #ddd; border-radius: 5px; font-size: 1em;" />
		<button onclick="openEvaluationGraph()" style="padding: 12px 24px; background: #235bf5; color: #fff; border: none; border-radius: 5px; cursor: pointer; font-weight: bold;">그래프 보기</button>
	</div>
	<p style="margin-top: 10px; font-size: 0.85em; color: #999;">평가 저장은 앱에서 진행됩니다. 이 페이지는 공개 그래프만 보여줍니다.</p>
</div>
<script>
function openEvaluationGraph() {
	var cc = document.getElementById('graphCc').value.trim().toLowerCase();
	if (!cc) { alert('도메인을 입력하세요.'); return; }
	location.href = '/graph/view?cc=' + encodeURIComponent(cc);
}
</script>
					
				</div>

				<a class="aside verify">
					<span class="tb">
						<span class="tc">
							<span class="text">
								<span>지금 무료로 사용해보세요</span>
								<em>시작하기</em>
							</span>
						</span>
					</span>
				</a>

				<div id="footer"></div>
			</div>

			<script>
				var cookies = ${JSON.stringify(cookies)}
				var rows = ${JSON.stringify(rows)}
			</script>
			<script src="/static/js/min.js"></script>
			<script src="/static/js/${ cookies.address ? 'script.js' : 'auth.js' }"></script>
		</body>
		</html>
	`;
}
