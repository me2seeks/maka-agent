/*
 * Licensed to the Apache Software Foundation (ASF) under one
 * or more contributor license agreements.  See the NOTICE file
 * distributed with this work for additional information
 * regarding copyright ownership.  The ASF licenses this file
 * to you under the Apache License, Version 2.0 (the
 * "License"); you may not use this file except in compliance
 * with the License.  You may obtain a copy of the License at
 *
 *     http://www.apache.org/licenses/LICENSE-2.0
 *
 * Unless required by applicable law or agreed to in writing,
 * software distributed under the License is distributed on an
 * "AS IS" BASIS, WITHOUT WARRANTIES OR CONDITIONS OF ANY
 * KIND, either express or implied.  See the License for the
 * specific language governing permissions and limitations
 * under the License.
 */

//! Simplified Chinese copy for the chat shell. Command identities and backend
//! diagnostics are not translated. Add another catalog only when a locale ships.

pub(crate) const CHAT_TITLE: &str = "Maka · 演示 · 未连接 Host";
pub(crate) const CHAT_NOTICE: &str = "演示模式：输入内容不会发送或保存";
pub(crate) const CHAT_INPUT: &str = "输入";
pub(crate) const CHAT_READ: &str = "输入 · F6 返回编辑";
pub(crate) const CHAT_HINT: &str = "/ 命令 · Enter 发送 · Shift+Enter 换行 · Ctrl+C 退出";
pub(crate) const CHAT_SEND_UNAVAILABLE: &str = "未连接 Host，无法发送；输入内容已保留";
pub(crate) const LATEST_EMPTY: &str = "当前没有历史消息；未连接 Host";
pub(crate) const LATEST_DONE: &str = "已回到最新消息";
pub(crate) const THINKING_EXPANDED: &str = "思考显示已设为展开（包括后续消息）";
pub(crate) const THINKING_FOLDED: &str = "思考显示已设为折叠（包括后续消息）";
pub(crate) const COMMAND_TITLE: &str = "命令";
pub(crate) const HOST_HELP: &str = "/（空输入框）或 Ctrl+P：搜索命令\nEnter：发送；Shift+Enter / Ctrl+J：换行\nCtrl+C：运行时停止，空闲时直接退出\nEsc：关闭弹层，返回编辑\nPgUp/PgDn：阅读；F6：切换焦点\nCtrl+T：展开/折叠思考；F4：回到最新\nF5：本地预览，不发送\nF3 或 /requests：权限、提问与表单\n断线保留输入，不自动重连或重发。";
pub(crate) const COMMAND_EMPTY: &str = "没有匹配的命令";
pub(crate) const COMMAND_RESIZE: &str = "请放大窗口查看命令\nEsc 返回";
pub(crate) const COMMAND_LIMIT: &str = "搜索上限：128 字节\n退格修改 · Esc 返回";
pub(crate) const COMMAND_INVALID: &str = "搜索输入被拒绝\n请检查控制字符";
pub(crate) const HELP_LABEL: &str = "/help     帮助";
pub(crate) const LATEST_LABEL: &str = "/latest   回到最新";
pub(crate) const THINKING_LABEL: &str = "/thinking 展开/折叠思考";
pub(crate) const PREVIEW_LABEL: &str = "/preview  本地预览";
pub(crate) const CHAT_HELP_TITLE: &str = "帮助 · 演示模式";
pub(crate) const CHAT_HELP: &str = "/（空输入框）或 Ctrl+P：搜索命令\n//：输入普通斜杠，例如文件路径\n方向键选择，Enter 确认，也可点击命令\nEsc：关闭弹层，输入保持不变\nEnter：发送（演示未连接，不能发送）\nShift+Enter / Ctrl+J：换行\nF5：本地预览，不发送\nCtrl+Z / Ctrl+Y：撤销 / 重做\nPgUp/PgDn：阅读；F6：切换焦点\nCtrl+C：退出。";
pub(crate) const CHAT_EMPTY: &str = "请输入内容";
pub(crate) const CHAT_NO_REPLAY: &str = "聊天演示没有自动回放；未连接 Host";
pub(crate) const CHAT_NO_REQUEST: &str = "没有待处理请求；未连接 Host";
pub(crate) const CHAT_RESIZE: &str = "请放大窗口查看\nEsc 返回";
pub(crate) const CHAT_INPUT_LIMITS: &str = "输入兼容性受限 · F1 查看帮助";

pub(crate) fn preview(bytes: usize) -> String {
    format!("本地预览：{bytes} 字节；未发送，输入已保留")
}

pub(crate) fn command_hint(selected: usize, total: usize, compact: bool) -> String {
    if compact {
        format!("↑↓ Enter {selected}/{total}\nEsc 返回")
    } else {
        format!("↑↓ Enter {selected}/{total} · Esc 返回\n命令名 / 中文搜索")
    }
}

pub(crate) fn edit_error(error: &crate::composer::EditError) -> String {
    use crate::composer::EditError;
    match error {
        EditError::TooLarge { max_bytes } => {
            format!("输入被拒绝：上限为 {max_bytes} 字节；原内容已保留")
        }
        EditError::InvalidControl { character } => {
            format!("输入被拒绝：不支持控制字符 {character:?}；原内容已保留")
        }
        EditError::RevisionExhausted => "输入被拒绝：编辑次数已达上限；原内容已保留".into(),
    }
}
