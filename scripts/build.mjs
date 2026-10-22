// Builds the extension into extension/dist:
//   1. the Rust core → WebAssembly (rustup's stable toolchain, which has wasm32)
//   2. wasm-bindgen glue (pinned, installed in .tools/)
//   3. esbuild bundles for the service worker, popup, dashboard and picker
//   4. static files (manifest, HTML, CSS, icons)
import { execFileSync } from 'node:child_process';
import { cpSync, existsSync, mkdirSync, rmSync, copyFileSync } from 'node:fs';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';
import * as esbuild from 'esbuild';

const root = fileURLToPath(new URL('..', import.meta.url));
const ext = join(root, 'extension');
const out = join(ext, 'dist');
const generated = join(ext, 'build', 'core');
const run = (cmd, args, env = {}) => execFileSync(cmd, args, { cwd: root, stdio: 'inherit', env: { ...process.env, ...env } });

// rustup's toolchain, not whatever cargo is first on PATH (Homebrew's has no wasm32).
const sysroot = execFileSync('rustup', ['run', 'stable', 'rustc', '--print', 'sysroot'], { encoding: 'utf8' }).trim();
const exe = name => join(sysroot, 'bin', process.platform === 'win32' ? `${name}.exe` : name);
run(exe('cargo'), ['build', '-p', 'pocket-scraper-core-wasm', '--target', 'wasm32-unknown-unknown', '--release', '--locked'], {
  RUSTC: exe('rustc'),
  // On macOS this toolchain's rust-lld looks for libLLVM next to itself; point it at the sysroot.
  ...(process.platform === 'darwin' ? { DYLD_FALLBACK_LIBRARY_PATH: join(sysroot, 'lib') } : {})
});

const bindgen = join(root, '.tools', 'bin', process.platform === 'win32' ? 'wasm-bindgen.exe' : 'wasm-bindgen');
if (!existsSync(bindgen)) throw new Error('wasm-bindgen is missing: cargo install --locked --root .tools wasm-bindgen-cli --version 0.2.129');
rmSync(generated, { recursive: true, force: true });
run(bindgen, ['--target', 'web', '--out-dir', generated, join(root, 'target', 'wasm32-unknown-unknown', 'release', 'pocket_scraper_core_wasm.wasm')]);

rmSync(out, { recursive: true, force: true });
mkdirSync(out, { recursive: true });
const common = { bundle: true, format: 'esm', target: 'chrome123', legalComments: 'none', logLevel: 'warning', charset: 'utf8' };
await esbuild.build({ ...common, entryPoints: { background: join(ext, 'src', 'background.ts'), popup: join(ext, 'src', 'popup.ts'), dashboard: join(ext, 'src', 'dashboard.ts') }, outdir: out });
// The picker is injected as a classic script, so it must not be a module.
await esbuild.build({ ...common, format: 'iife', entryPoints: { picker: join(ext, 'src', 'picker.ts') }, outdir: out });
copyFileSync(join(generated, 'pocket_scraper_core_wasm_bg.wasm'), join(out, 'core.wasm'));
cpSync(join(ext, 'static'), out, { recursive: true });
console.log(`Built ${out}`);
