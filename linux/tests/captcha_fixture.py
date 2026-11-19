#!/usr/bin/env python3
"""Локальная страница-заглушка для проверки helpers/csqtt-captcha.

Имитирует только транспорт: страница сама вызывает
/method/captchaNotRobot.check (fetch или XHR — параметр ?via=), сервер
отвечает как VK. Настоящую проверку VK здесь никто не проходит.
"""
import json
import sys
from http.server import BaseHTTPRequestHandler, HTTPServer

PAGE = """<!doctype html><meta charset=utf-8><title>fixture</title>
<p id=state>ждём…</p>
<script>
const via = new URLSearchParams(location.search).get('via') || 'fetch';
const outcome = new URLSearchParams(location.search).get('outcome') || 'ok';
const url = '/method/captchaNotRobot.check?outcome=' + outcome;
setTimeout(() => {
  if (via === 'xhr') {
    const x = new XMLHttpRequest(); x.open('POST', url); x.send('a=1');
  } else {
    fetch(url, {method: 'POST', body: 'a=1'}).then(r => r.json());
  }
}, 300);
</script>"""


class Handler(BaseHTTPRequestHandler):
    def log_message(self, *_):
        pass

    def reply(self, body, kind):
        data = body.encode()
        self.send_response(200)
        self.send_header("Content-Type", kind)
        self.send_header("Content-Length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)

    def do_GET(self):
        self.reply(PAGE, "text/html; charset=utf-8")

    def do_POST(self):
        if "outcome=error" in self.path:
            self.reply(json.dumps({"error": {"error_code": 14, "error_msg": "bad"}}), "application/json")
        else:
            self.reply(json.dumps({"response": {"status": "OK", "success_token": "tok_fixture_123"}}), "application/json")


if __name__ == "__main__":
    HTTPServer(("127.0.0.1", int(sys.argv[1])), Handler).serve_forever()
