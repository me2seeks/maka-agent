import { packager } from '@electron/packager';
import { execFile } from 'node:child_process';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { promisify } from 'node:util';

const root = path.dirname(fileURLToPath(import.meta.url));
const productName = 'Maka Voice Permission Probe';
const appBundleId = 'io.github.me2seeks.maka-voice-permission-probe';
const execFileAsync = promisify(execFile);
const signingIdentity = process.env.MAKA_MIC_PROBE_SIGN_IDENTITY || '-';

const outputPaths = await packager({
  dir: root,
  out: path.join(root, 'out'),
  overwrite: true,
  platform: process.platform,
  arch: process.arch,
  name: productName,
  executableName: productName,
  appBundleId,
  appVersion: '0.1.0',
  electronVersion: '43.1.1',
  asar: true,
  prune: true,
  usageDescription: {
    Microphone:
      'Records about two seconds locally to verify microphone permission behavior. Audio is not saved, played back, or uploaded.',
  },
  osxSign:
    process.platform === 'darwin'
      ? {
          identity: signingIdentity,
          identityValidation: signingIdentity !== '-',
          preAutoEntitlements: false,
          timestamp: signingIdentity === '-' ? 'none' : undefined,
          continueOnError: false,
          optionsForFile: (filePath) =>
            path.basename(filePath) === `${productName}.app`
              ? { entitlements: path.join(root, 'entitlements.mac.plist') }
              : {},
        }
      : undefined,
  ignore: [
    /^\/node_modules(?:\/|$)/,
    /^\/out(?:\/|$)/,
    /^\/test(?:\/|$)/,
    /^\/README\.zh-CN\.md$/,
    /^\/entitlements\.mac\.plist$/,
    /^\/pack\.mjs$/,
    /^\/\.gitignore$/,
    /^\/linux-smoke\.png$/,
  ],
});

if (process.platform === 'darwin') {
  for (const outputPath of outputPaths) {
    const appPath = path.join(outputPath, `${productName}.app`);
    const plistPath = path.join(appPath, 'Contents', 'Info.plist');
    const { stdout: bundleId } = await execFileAsync('/usr/libexec/PlistBuddy', [
      '-c',
      'Print :CFBundleIdentifier',
      plistPath,
    ]);
    const { stdout: usageDescription } = await execFileAsync(
      '/usr/libexec/PlistBuddy',
      ['-c', 'Print :NSMicrophoneUsageDescription', plistPath],
    );
    if (bundleId.trim() !== appBundleId || usageDescription.trim().length === 0) {
      throw new Error('Packaged macOS permission metadata verification failed');
    }
    await execFileAsync('/usr/bin/codesign', [
      '--verify',
      '--deep',
      '--strict',
      '--verbose=2',
      appPath,
    ]);
    const entitlementResult = await execFileAsync('/usr/bin/codesign', [
      '--display',
      '--entitlements',
      ':-',
      appPath,
    ]);
    const entitlements = `${entitlementResult.stdout}\n${entitlementResult.stderr}`;
    if (!entitlements.includes('com.apple.security.device.audio-input')) {
      throw new Error('Packaged macOS app is missing the audio-input entitlement');
    }
  }
}

process.stdout.write(`Packaged application:\n${outputPaths.join('\n')}\n`);
