'use strict';

const { contextBridge, ipcRenderer } = require('electron');

const IPC = Object.freeze({
  getSnapshot: 'probe:get-snapshot',
  askMacPermission: 'probe:ask-mac-permission',
  copyReport: 'probe:copy-report',
});

contextBridge.exposeInMainWorld(
  'makaMicProbe',
  Object.freeze({
    getSnapshot: () => ipcRenderer.invoke(IPC.getSnapshot),
    askMacPermission: () => ipcRenderer.invoke(IPC.askMacPermission),
    copyReport: (text) => ipcRenderer.invoke(IPC.copyReport, text),
  }),
);
