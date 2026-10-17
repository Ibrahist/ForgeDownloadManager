#!/usr/bin/env node

/**
 * Packages the Forge Download Manager MV3 browser extension into a zip archive
 * suitable for Chrome Web Store, Firefox Add-ons (AMO), Edge Add-ons, or manual loading.
 */

import fs from 'node:fs';
import path from 'node:path';
import crypto from 'node:crypto';
import { execSync } from 'node:child_process';
import { fileURLToPath } from 'node:url';

const __filename = fileURLToPath(import.meta.url);
const __dirname = path.dirname(__filename);
const rootDir = path.resolve(__dirname, '..');
const extDir = path.join(rootDir, 'apps', 'extension');
const distDir = path.join(rootDir, 'dist');
const zipOut = path.join(distDir, 'forge-dm-extension.zip');

console.log('==> Packaging Forge Download Manager Browser Extension...');

// 1. Verify manifest exists and is valid JSON
const manifestPath = path.join(extDir, 'manifest.json');
if (!fs.existsSync(manifestPath)) {
  console.error(`Error: Manifest not found at ${manifestPath}`);
  process.exit(1);
}

const manifest = JSON.parse(fs.readFileSync(manifestPath, 'utf8'));
console.log(`    Extension: ${manifest.name} v${manifest.version}`);
console.log(`    Manifest Version: ${manifest.manifest_version}`);

// 2. Prepare output directory
fs.mkdirSync(distDir, { recursive: true });
if (fs.existsSync(zipOut)) {
  fs.unlinkSync(zipOut);
}

// 3. Compress extension files into zip archive
const isWindows = process.platform === 'win32';
const filesToPack = ['manifest.json', 'background', 'content', 'popup', 'options', 'icons'];

for (const item of filesToPack) {
  const itemPath = path.join(extDir, item);
  if (!fs.existsSync(itemPath)) {
    console.error(`Error: Missing required component: ${item}`);
    process.exit(1);
  }
}

try {
  if (isWindows) {
    // Use PowerShell Compress-Archive for zero-dependency zip creation on Windows
    const psCmd = `Compress-Archive -Path '${filesToPack.map(f => path.join(extDir, f)).join("','")}' -DestinationPath '${zipOut}' -Force`;
    execSync(`powershell -NoProfile -Command "${psCmd}"`, { stdio: 'inherit' });
  } else {
    // Use zip command on macOS/Linux
    execSync(`cd "${extDir}" && zip -r "${zipOut}" ${filesToPack.join(' ')}`, { stdio: 'inherit' });
  }
} catch (err) {
  console.error('Failed to create zip archive:', err.message);
  process.exit(1);
}

// 4. Compute SHA256 checksum
const zipBytes = fs.readFileSync(zipOut);
const hash = crypto.createHash('sha256').update(zipBytes).digest('hex');
const sizeKb = (zipBytes.length / 1024).toFixed(2);

console.log(`\n==> Extension successfully packaged!`);
console.log(`    Archive:  ${zipOut}`);
console.log(`    Size:     ${sizeKb} KB`);
console.log(`    SHA256:   ${hash}`);
