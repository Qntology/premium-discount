// 화면을 dist/에 묶는다: main.ts(+commands.ts, localdb.ts, view.ts, dexie)를 main.js 하나로, index.html과 style.css는 그대로 복사한다.
import { build } from 'esbuild'
import { copyFileSync, mkdirSync } from 'node:fs'
import { fileURLToPath } from 'node:url'

const here = (p) => fileURLToPath(new URL(p, import.meta.url))
mkdirSync(here('dist'), { recursive: true })
await build({ entryPoints: [here('main.ts')], bundle: true, format: 'esm', target: 'es2022', outfile: here('dist/main.js'), sourcemap: true, logLevel: 'info' })
for (const f of ['index.html', 'style.css']) copyFileSync(here(f), here(`dist/${f}`))
