'use strict';

const { app, BrowserWindow, clipboard, ipcMain, systemPreferences } = require('electron');
const os = require('node:os');
const path = require('node:path');
const { pathToFileURL } = require('node:url');
const {
  assertMacPermissionRequestAllowed,
  evaluatePermission,
  parseProbeConfiguration,
} = require('./probe-policy.cjs');

const IPC = Object.freeze({
  getSnapshot: 'probe:get-snapshot',
  askMacPermission: 'probe:ask-mac-permission',
  copyReport: 'probe:copy-report',
});
const MAX_PERMISSION_EVENTS = 50;
const MAX_CLIPBOARD_CHARS = 100_000;

let configuration;
try {
  configuration = parseProbeConfiguration(process.argv);
} catch (error) {
  process.stderr.write(`${error instanceof Error ? error.message : String(error)}\n`);
  process.exit(2);
}

let mainWindow = null;
let trustedUrl = '';
const permissionEvents = [];

function nativeMicrophonePermission() {
  if (process.platform !== 'darwin' && process.platform !== 'win32') {
    return { supported: false, status: 'unsupported' };
  }

  try {
    return {
      supported: true,
      status: systemPreferences.getMediaAccessStatus('microphone'),
    };
  } catch (error) {
    return {
      supported: true,
      status: 'unknown',
      errorName: error instanceof Error ? error.name : 'Error',
    };
  }
}

function runtimeSnapshot() {
  return {
    runtime: {
      platform: process.platform,
      osRelease: os.release(),
      osVersion: typeof os.version === 'function' ? os.version() : null,
      arch: process.arch,
      electronVersion: process.versions.electron,
      chromeVersion: process.versions.chrome,
      nodeVersion: process.versions.node,
      appVersion: app.getVersion(),
      isPackaged: app.isPackaged,
    },
    configuration,
    nativeMicrophonePermission: nativeMicrophonePermission(),
    permissionHandlerEvents:
      configuration.sessionPolicy === 'default' ? null : permissionEvents.slice(),
  };
}

function recordPermissionEvent(event) {
  permissionEvents.push(event);
  if (permissionEvents.length > MAX_PERMISSION_EVENTS) permissionEvents.shift();
}

function installStrictPermissionHandlers(window) {
  if (configuration.sessionPolicy === 'default') return;

  const ses = window.webContents.session;
  const trustedWebContentsId = window.webContents.id;

  ses.setPermissionCheckHandler(
    (webContents, permission, requestingOrigin, details) => {
      const result = evaluatePermission({
        phase: 'check',
        sessionPolicy: configuration.sessionPolicy,
        permission,
        webContentsId: webContents?.id ?? null,
        trustedWebContentsId,
        currentUrl: webContents?.getURL() ?? '',
        trustedUrl,
        requestingOrigin,
        details,
      });
      recordPermissionEvent(result.event);
      return result.allowed;
    },
  );

  ses.setPermissionRequestHandler(
    (webContents, permission, callback, details) => {
      const result = evaluatePermission({
        phase: 'request',
        sessionPolicy: configuration.sessionPolicy,
        permission,
        webContentsId: webContents.id,
        trustedWebContentsId,
        currentUrl: webContents.getURL(),
        trustedUrl,
        requestingOrigin: details.securityOrigin,
        details,
      });
      recordPermissionEvent(result.event);
      callback(result.allowed);
    },
  );
}

function isTrustedIpcSender(event) {
  return (
    mainWindow !== null &&
    !mainWindow.isDestroyed() &&
    event.sender.id === mainWindow.webContents.id &&
    event.senderFrame?.url === trustedUrl
  );
}

function handleTrusted(channel, handler) {
  ipcMain.handle(channel, async (event, ...args) => {
    if (!isTrustedIpcSender(event)) throw new Error('Untrusted IPC sender');
    return handler(...args);
  });
}

function registerIpcHandlers() {
  handleTrusted(IPC.getSnapshot, async () => runtimeSnapshot());

  handleTrusted(IPC.askMacPermission, async () => {
    assertMacPermissionRequestAllowed(configuration);

    const before = nativeMicrophonePermission();
    if (process.platform !== 'darwin') {
      return {
        supported: false,
        before,
        granted: null,
        after: nativeMicrophonePermission(),
      };
    }

    try {
      const granted = await systemPreferences.askForMediaAccess('microphone');
      return {
        supported: true,
        before,
        granted,
        after: nativeMicrophonePermission(),
        error: null,
      };
    } catch (error) {
      return {
        supported: true,
        before,
        granted: null,
        after: nativeMicrophonePermission(),
        error: {
          name: error instanceof Error ? error.name : 'Error',
          message:
            error instanceof Error
              ? error.message.slice(0, 500)
              : String(error).slice(0, 500),
        },
      };
    }
  });

  handleTrusted(IPC.copyReport, async (text) => {
    if (typeof text !== 'string' || text.length > MAX_CLIPBOARD_CHARS) {
      throw new Error('Invalid report text');
    }
    clipboard.writeText(text);
    return true;
  });
}

async function createWindow() {
  const indexPath = path.join(__dirname, 'index.html');
  trustedUrl = pathToFileURL(indexPath).href;

  mainWindow = new BrowserWindow({
    width: 960,
    height: 800,
    minWidth: 720,
    minHeight: 640,
    title: 'Maka 麦克风权限验证器',
    backgroundColor: '#f5f7fb',
    webPreferences: {
      preload: path.join(__dirname, 'preload.cjs'),
      contextIsolation: true,
      nodeIntegration: false,
      sandbox: true,
      webSecurity: true,
      allowRunningInsecureContent: false,
    },
  });

  installStrictPermissionHandlers(mainWindow);

  mainWindow.webContents.session.webRequest.onBeforeRequest(
    { urls: ['http://*/*', 'https://*/*', 'ws://*/*', 'wss://*/*'] },
    (_details, callback) => callback({ cancel: true }),
  );
  mainWindow.webContents.setWindowOpenHandler(() => ({ action: 'deny' }));
  mainWindow.webContents.on('will-navigate', (event, url) => {
    if (url !== trustedUrl) event.preventDefault();
  });
  mainWindow.on('closed', () => {
    mainWindow = null;
  });

  await mainWindow.loadFile(indexPath);
}

app.whenReady().then(async () => {
  registerIpcHandlers();
  await createWindow();

  app.on('activate', async () => {
    if (BrowserWindow.getAllWindows().length === 0) await createWindow();
  });
});

app.on('window-all-closed', () => {
  if (process.platform !== 'darwin') app.quit();
});
