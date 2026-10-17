#!/usr/bin/env node
/**
 * hvigor 签名密码的加解密工具（用于 CI 自备签名材料时把明文密码转成 hvigor 要求的加密串）。
 *
 * 背景（逆向自 hvigor-ohos-plugin/src/utils/decipher-util.js，实测可用）：
 *   - hvigor 6.26.4 只接受 DevEco 加密后的十六进制串（长度 ≥32），明文直接报
 *     00303116 "The length of the storePassword or keyPassword field ... is less than 32"。
 *   - 密钥材料目录取的是 **p12 所在目录下的 material/**：
 *       material/fd/{0,1,2}/<file>  每个文件 16 字节，三个分量互异或后再异或内置 component
 *       material/ce/<file>          rootKey 用 AES-128-GCM 加密出来的 workKey
 *       material/ac/<file>          PBKDF2 的 salt（10000 轮 SHA-256 → 16 字节 rootKey）
 *     注意目录名与用途是**反的**（照抄 hvigor 的 dirs = ['fd','ac','ce']，
 *     它把 dirs[1]='ac' 当 salt、dirs[2]='ce' 当 work material）：
 *        rootKey = PBKDF2(xor(fd0,fd1,fd2,component).toString(), ac, 10000, 16, 'sha256')
 *        workKey = GCM_decrypt(rootKey, ce)
 *        密文串  = '0000001A' + iv(26B) + GCM_encrypt(workKey, pwd) + tag(16B)   （hex）
 *
 * 用法：
 *   node scripts/ohos-sign-pwd.js init    <materialDir>            # 生成一套全新的 material/
 *   node scripts/ohos-sign-pwd.js encrypt <p12Dir> <plaintext>     # 明文 → hvigor 加密串
 *   node scripts/ohos-sign-pwd.js decrypt <p12Dir> <hexString>     # 反向自检
 *   （<p12Dir> 是 p12 所在目录，即 material/ 的父目录）
 */
'use strict';
const fs = require('fs');
const path = require('path');
const crypto = require('crypto');

const COMPONENT = new Int8Array([49, 243, 9, 115, 214, 175, 91, 184, 211, 190, 177, 88, 101, 131, 192, 119]);
const DIRS = ['fd', 'ac', 'ce'];

function oneFile(dir) {
  const files = fs.readdirSync(dir).filter((f) => f !== '.DS_Store');
  if (files.length !== 1) throw new Error(`${dir}: expected exactly 1 file, got ${files.length}`);
  return new Int8Array(fs.readFileSync(path.join(dir, files[0])));
}
function readFd(materialDir) {
  const fd = path.join(materialDir, DIRS[0]);
  return fs.readdirSync(fd).filter((f) => f !== '.DS_Store')
    .map((sub) => oneFile(path.join(fd, sub)));
}
function xor(a, b) {
  const out = new Int8Array(a.byteLength);
  for (let i = 0; i < a.byteLength; i++) out[i] = a[i] ^ b[i];
  return out;
}
function xorAll(list) {
  let acc = xor(list[0], list[1]);
  for (let i = 2; i < list.length; i++) acc = xor(acc, list[i]);
  return acc;
}
function rootKey(materialDir) {
  const fd = readFd(materialDir);
  const salt = oneFile(path.join(materialDir, DIRS[1])); // ac/ 是 salt（名字与用途是反的）
  const mixed = xorAll(fd.concat([COMPONENT]));
  return new Int8Array(crypto.pbkdf2Sync(Buffer.from(mixed).toString(), salt, 1e4, 16, 'sha256'));
}
function gcmDecrypt(key, buf) {
  const h = ((buf[0] & 255) << 24) | ((buf[1] & 255) << 16) | ((buf[2] & 255) << 8) | (buf[3] & 255);
  const ivLen = buf.length - 4 - h;          // hvigor 就是这么切的
  const iv = buf.subarray(4, 4 + ivLen);
  const ct = buf.subarray(4 + ivLen, buf.length - 16);
  const tag = buf.subarray(buf.length - 16);
  const d = crypto.createDecipheriv('aes-128-gcm', key, iv);
  d.setAuthTag(tag);
  return Buffer.concat([d.update(ct), d.final()]);
}
function gcmEncrypt(key, plain, ivLen = 12) {
  const iv = crypto.randomBytes(ivLen);
  const c = crypto.createCipheriv('aes-128-gcm', key, iv);
  const ct = Buffer.concat([c.update(plain), c.final()]);
  const head = Buffer.alloc(4);
  head.writeInt32BE(ct.length + 16, 0);         // h = 密文长度 + 16（DevEco 的习惯值）
  return Buffer.concat([head, iv, ct, c.getAuthTag()]);
}
function workKey(materialDir) {
  return new Int8Array(gcmDecrypt(rootKey(materialDir), Buffer.from(oneFile(path.join(materialDir, DIRS[2]))))); // ce/ 是 workKey
}

const [cmd, arg1, arg2] = process.argv.slice(2);
if (cmd === 'init') {
  const dir = arg1;
  const mat = path.join(dir, 'material');
  for (const d of ['fd/0', 'fd/1', 'fd/2', 'ac', 'ce']) fs.mkdirSync(path.join(mat, d), { recursive: true });
  for (const d of ['fd/0', 'fd/1', 'fd/2']) fs.writeFileSync(path.join(mat, d, crypto.randomBytes(8).toString('hex')), crypto.randomBytes(16));
  fs.writeFileSync(path.join(mat, 'ac', crypto.randomBytes(8).toString('hex')), crypto.randomBytes(16)); // salt
  // ce 必须最后写：它要用 rootKey 加密 workKey，而 rootKey 依赖 fd + ac(salt)
  const wk = crypto.randomBytes(16);
  fs.writeFileSync(path.join(mat, 'ce', crypto.randomBytes(8).toString('hex')), gcmEncrypt(rootKey(mat), wk));
  console.log(`[ohos-sign-pwd] fresh material written to ${mat}`);
  console.log(`[ohos-sign-pwd] p12/cer/p7b 请放在 ${dir}/ 下（material/ 的父目录 = p12 所在目录）`);
} else if (cmd === 'encrypt') {
  const wk = workKey(path.join(arg1, 'material'));
  console.log(gcmEncrypt(wk, Buffer.from(arg2, 'utf-8')).toString('hex'));
} else if (cmd === 'decrypt') {
  const wk = workKey(path.join(arg1, 'material'));
  console.log(gcmDecrypt(wk, Buffer.from(arg2, 'hex')).toString('utf-8'));
} else {
  console.error('usage: ohos-sign-pwd.js init <p12Dir> | encrypt <p12Dir> <plaintext> | decrypt <p12Dir> <hex>');
  process.exit(2);
}