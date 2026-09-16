# Licensed to the Apache Software Foundation (ASF) under one
# or more contributor license agreements.  See the NOTICE file
# distributed with this work for additional information
# regarding copyright ownership.  The ASF licenses this file
# to you under the Apache License, Version 2.0 (the
# "License"); you may not use this file except in compliance
# with the License.  You may obtain a copy of the License at
#
#     http://www.apache.org/licenses/LICENSE-2.0
#
# Unless required by applicable law or agreed to in writing,
# software distributed under the License is distributed on an
# "AS IS" BASIS, WITHOUT WARRANTIES OR CONDITIONS OF ANY
# KIND, either express or implied.  See the License for the
# specific language governing permissions and limitations
# under the License.

"""Developer launcher acceptance, using only its temporary FakeBackend Host."""

import sys
from pty_smoke import Session
from session_host_pty import expect_compact

session = Session([sys.argv[1], sys.argv[2], '--isolated'])
try:
    session.expect_text('FakeBackend')
    session.expect_text('已连接')
    session.send(b'isolated-launcher-test\r')
    expect_compact(session, 'rendererloopareconnected.')
    session.pump(0.5)
    session.send(b'\x03')
    session.completed()
    session.expect_text('临时测试数据已清理')
finally:
    session.close()
print('PASS: isolated Host launch, send, response, terminal restoration and cleanup')
