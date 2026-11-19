#!/usr/bin/env python3
"""Сверка формата ссылок csqtt:// с независимой реализацией.

Алгоритм здесь записан по Kotlin-коду Android-клиента (UiUtils.kt:
encryptCsqttPayload / decryptCsqttPayload) на библиотеке cryptography, без
общего кода с Rust-реализацией. Скрипт проверяет обе стороны:
ссылка «от Android» читается утилитой csqtt, а ссылка от csqtt
расшифровывается «Android-стороной» в ожидаемый plaintext.

    python3 tests/link_compat.py target/debug/csqtt
"""
import base64
import hashlib
import hmac
import os
import subprocess
import sys
import tempfile

from cryptography.hazmat.primitives.ciphers.aead import AESGCM

KEY = hashlib.sha256(b"csqtt-chiper-key").digest()


def kotlin_encrypt(plain: str) -> str:
    iv = hmac.new(KEY, plain.encode(), hashlib.sha256).digest()[:12]
    sealed = AESGCM(KEY).encrypt(iv, plain.encode(), None)
    return "csqtt://" + base64.urlsafe_b64encode(iv + sealed).rstrip(b"=").decode()


def kotlin_decrypt(link: str) -> str:
    body = link[len("csqtt://"):]
    raw = base64.urlsafe_b64decode(body + "=" * (-len(body) % 4))
    return AESGCM(KEY).decrypt(raw[:12], raw[12:], None).decode()


def main() -> int:
    csqtt = sys.argv[1]
    failures = 0
    with tempfile.TemporaryDirectory() as config:
        env = dict(os.environ, CSQTT_CONFIG_DIR=config)

        def run(*args):
            return subprocess.run([csqtt, *args], env=env, capture_output=True, text=True)

        plain = "connect?v=2&host=203.0.113.7&peer=46000&password=SecretPass123456&hashes=abcdefghijklmnop1+abcdefghijklmno%2Bp"
        result = run("import", kotlin_encrypt(plain), "--name", "android")
        shown = run("show", "android", "--secrets").stdout
        ok = (
            result.returncode == 0
            and 'host = "203.0.113.7"' in shown
            and "port = 46000" in shown
            and 'password = "SecretPass123456"' in shown
            and '"abcdefghijklmnop1"' in shown
            and '"abcdefghijklmno+p"' in shown
        )
        print("ok  " if ok else "FAIL", "ссылка Android-формата импортируется")
        failures += not ok

        exported = run("export", "android").stdout.strip()
        try:
            decrypted = kotlin_decrypt(exported)
        except Exception as error:  # noqa: BLE001 - любая ошибка расшифровки = провал
            decrypted = "<%s>" % error
        ok = decrypted == plain
        print("ok  " if ok else "FAIL", "экспорт расшифровывается Android-алгоритмом:", decrypted)
        failures += not ok

        # Детерминированный IV: тот же plaintext даёт побайтно ту же ссылку.
        ok = exported == kotlin_encrypt(plain)
        print("ok  " if ok else "FAIL", "ссылка побайтно совпадает с Android-вариантом")
        failures += not ok

        token_plain = "connect?v=2&host=vps.example&peer=46001&password=p%40ss&token=vk1.a.AbC-_123"
        result = run("import", kotlin_encrypt(token_plain), "--name", "token")
        shown = run("show", "token", "--secrets").stdout
        ok = result.returncode == 0 and 'vk_token = "vk1.a.AbC-_123"' in shown and 'password = "p@ss"' in shown
        print("ok  " if ok else "FAIL", "ссылка с VK-токеном импортируется")
        failures += not ok

        result = run("import", kotlin_encrypt("connect?v=2&host=127.0.0.1&peer=46000&password=x"))
        ok = result.returncode != 0
        print("ok  " if ok else "FAIL", "ссылка на localhost отклоняется")
        failures += not ok
    return 1 if failures else 0


if __name__ == "__main__":
    sys.exit(main())
