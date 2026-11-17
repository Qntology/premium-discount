// 데스크톱 설치본에 넣을 사이드카 두 개를 만들어 Tauri 가 찾는 이름으로 둔다.
//
//   premium-discount-app  헤드리스 코어 (serve_lines, ../Cargo.toml)
//   premium-discount-mcp  Raon Zena 가 부르는 MCP 서버 (../../crates/premium-discount-mcp)
//
// Tauri 의 bundle.externalBin 은 빌드할 때 `binaries/<이름>-<대상 삼중항>[.exe]` 를 찾고, 설치할 때 삼중항을 떼어
// 창 실행 파일 옆에 둔다(%LOCALAPPDATA%\premium.discount\premium-discount-app.exe …). 그래서 raon-zena 의
// premium-discount-mcp 는 설치 폴더에서 코어를 찾는다(find_pd_app).
//
// 쓰는 곳: tauri.sidecar.conf.json 의 beforeBuildCommand (`tauri build --config src-tauri/tauri.sidecar.conf.json`).
// 사이드카는 기본 설정(tauri.conf.json)에 넣지 않는다: tauri-build 는 externalBin 파일이 없으면 `cargo build` 자체를
// 멈추므로, 사이드카 없이 창만 고치는 개발 빌드가 깨진다.
//
// 사용: node src-tauri/sidecars.mjs [--no-build] [--target <삼중항>]
//   --no-build  이미 만든 release 실행 파일을 옮기기만 한다
//   --target    교차 빌드 대상 (기본: rustc 의 host)

import { execFileSync } from 'node:child_process'
import { copyFileSync, existsSync, mkdirSync } from 'node:fs'
import { dirname, join, resolve } from 'node:path'
import { fileURLToPath } from 'node:url'

const here = dirname(fileURLToPath(import.meta.url))
const appDir = resolve(here, '..')
const cratesDir = resolve(here, '../../crates')
const args = process.argv.slice(2)
const noBuild = args.includes('--no-build')
const targetFlag = args.indexOf('--target')
const target =
  targetFlag >= 0
    ? args[targetFlag + 1]
    : execFileSync('rustc', ['-vV'], { encoding: 'utf8' })
        .split('\n')
        .find((l) => l.startsWith('host:'))
        .slice(5)
        .trim()
const exe = target.includes('windows') ? '.exe' : ''

const sidecars = [
  { name: 'premium-discount-app', manifest: join(appDir, 'Cargo.toml'), pkg: 'premium-discount-app', dir: appDir },
  { name: 'premium-discount-mcp', manifest: join(cratesDir, 'Cargo.toml'), pkg: 'premium-discount-mcp', dir: cratesDir },
]

const out = join(here, 'binaries')
mkdirSync(out, { recursive: true })
for (const s of sidecars) {
  const crossArgs = targetFlag >= 0 ? ['--target', target] : []
  if (!noBuild) {
    console.log(`[sidecars] cargo build --release -p ${s.pkg} --bin ${s.name} ${crossArgs.join(' ')}`)
    execFileSync('cargo', ['build', '--release', '--manifest-path', s.manifest, '-p', s.pkg, '--bin', s.name, ...crossArgs], {
      stdio: 'inherit',
    })
  }
  const built = join(s.dir, 'target', ...(targetFlag >= 0 ? [target] : []), 'release', s.name + exe)
  if (!existsSync(built)) {
    console.error(`[sidecars] ${built} 이 없습니다 (--no-build 이면 먼저 cargo build --release 를 하세요)`)
    process.exit(1)
  }
  const dest = join(out, `${s.name}-${target}${exe}`)
  copyFileSync(built, dest)
  console.log(`[sidecars] ${built} -> ${dest}`)
}
