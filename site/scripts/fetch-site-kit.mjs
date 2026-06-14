// Sparse-fetch the shared @weft/site-kit into ./vendor/site-kit.
//
// The kit lives in a SUBDIRECTORY (packages/site-kit) of a DIFFERENT repo
// (foundryside-dev/weft). npm cannot install a git *subdirectory* as a
// dependency directly, so we sparse-checkout just that subtree from the weft
// repo and vendor it locally. package.json then resolves it as
// "@weft/site-kit": "file:./vendor/site-kit".
//
// This is the sanctioned realization of the "git subdirectory dependency"
// decision (IA §1.3): NOT a published registry package, NOT a git submodule,
// and NOT a hand-committed static copy — vendor/site-kit/ is regenerated on
// every install/build and is gitignored. Runs from the "preinstall" hook so the
// vendor copy exists before npm resolves the file: dependency.
//
// Override the source for local development against an in-progress kit:
//   WEFT_REPO_URL   — git URL to clone (default: the public weft repo)
//   WEFT_REPO_REF   — branch/tag/sha to fetch (default: main)
//   WEFT_KIT_LOCAL  — path to a local weft checkout; copies
//                     <path>/packages/site-kit directly, skipping the clone.
import { cp, mkdir, rm, stat } from 'node:fs/promises';
import { existsSync } from 'node:fs';
import { execFileSync } from 'node:child_process';
import { fileURLToPath } from 'node:url';
import { dirname, join } from 'node:path';
import { tmpdir } from 'node:os';

const here = dirname(fileURLToPath(import.meta.url));
const siteRoot = join(here, '..');
const dest = join(siteRoot, 'vendor', 'site-kit');

const REPO_URL = process.env.WEFT_REPO_URL || 'https://github.com/foundryside-dev/weft.git';
const REPO_REF = process.env.WEFT_REPO_REF || 'main';
const KIT_SUBDIR = 'packages/site-kit';

function run(cmd, args, opts = {}) {
  execFileSync(cmd, args, { stdio: 'inherit', ...opts });
}

async function isDir(p) {
  try {
    return (await stat(p)).isDirectory();
  } catch {
    return false;
  }
}

async function vendorFrom(srcKit) {
  if (!(await isDir(srcKit))) {
    throw new Error(`[fetch-site-kit] kit not found at ${srcKit}`);
  }
  await rm(dest, { recursive: true, force: true });
  await mkdir(dirname(dest), { recursive: true });
  await cp(srcKit, dest, { recursive: true });
  console.log(`[fetch-site-kit] vendored ${srcKit} -> ${dest}`);
}

async function main() {
  // 1) Local checkout override — fast path for working against an unpushed kit.
  if (process.env.WEFT_KIT_LOCAL) {
    await vendorFrom(join(process.env.WEFT_KIT_LOCAL, KIT_SUBDIR));
    return;
  }

  // 2) Sparse, blobless, shallow clone of just packages/site-kit from the weft repo.
  const tmp = join(tmpdir(), `weft-site-kit-${process.pid}-${Date.now()}`);
  try {
    run('git', [
      'clone',
      '--depth', '1',
      '--filter=blob:none',
      '--sparse',
      '--branch', REPO_REF,
      REPO_URL,
      tmp,
    ]);
    run('git', ['sparse-checkout', 'set', KIT_SUBDIR], { cwd: tmp });
    await vendorFrom(join(tmp, KIT_SUBDIR));
  } finally {
    await rm(tmp, { recursive: true, force: true });
  }
}

main().catch((err) => {
  console.error(String(err && err.message ? err.message : err));
  // If a vendor copy already exists (e.g. offline rebuild), don't hard-fail the
  // whole install over a refetch — warn and continue with what is on disk.
  if (existsSync(join(dest, 'package.json'))) {
    console.error('[fetch-site-kit] continuing with the existing vendored copy.');
    process.exit(0);
  }
  process.exit(1);
});
