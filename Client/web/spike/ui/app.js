// Every network attempt below must fail; the only thing that may work is ivmpSend.
document.getElementById('origin').textContent = location.href;

const results = document.getElementById('results');
function report(name, outcome, good) {
  const row = document.createElement('div');
  row.className = 'row ' + (good ? 'ok' : 'bad');
  row.textContent = name + ': ' + outcome;
  results.appendChild(row);
  console.log('TEST ' + name + ' -> ' + outcome + (good ? ' (as wanted)' : ' (LEAK)'));
}
const expectBlocked = (name, promise) =>
  promise.then(() => report(name, 'REACHED THE INTERNET', false), (e) => report(name, 'blocked (' + (e && e.message || e) + ')', true));

// The bridge: the page's one way out.
function ivmpSend(name, data) {
  return new Promise((resolve, reject) => window.cefQuery({
    request: JSON.stringify({ name, data }),
    onSuccess: resolve,
    onFailure: (code, message) => reject(new Error(message)),
  }));
}

expectBlocked('fetch https', fetch('https://example.com/'));
expectBlocked('fetch http', fetch('http://example.com/'));
expectBlocked('fetch ip', fetch('http://1.1.1.1/'));
expectBlocked('xhr', new Promise((resolve, reject) => {
  const x = new XMLHttpRequest();
  x.open('GET', 'https://example.com/');
  x.onload = resolve; x.onerror = () => reject(new Error('xhr error'));
  x.send();
}));
expectBlocked('websocket', new Promise((resolve, reject) => {
  const ws = new WebSocket('wss://echo.websocket.events/');
  ws.onopen = resolve; ws.onerror = () => reject(new Error('socket error'));
}));
expectBlocked('image', new Promise((resolve, reject) => {
  const img = new Image();
  img.onload = resolve; img.onerror = () => reject(new Error('image error'));
  img.src = 'https://www.google.com/favicon.ico';
}));
expectBlocked('script', new Promise((resolve, reject) => {
  const s = document.createElement('script');
  s.onload = resolve; s.onerror = () => reject(new Error('script error'));
  s.src = 'https://cdn.jsdelivr.net/npm/lodash/lodash.min.js';
  document.head.appendChild(s);
}));

// The bridge must work.
ivmpSend('spike.hello', { from: 'page' }).then(
  (answer) => report('bridge ivmpSend', 'answered "' + answer + '"', true),
  (e) => report('bridge ivmpSend', 'failed: ' + e.message, false));

// Something moving, so repaints are visible in the log.
let n = 0;
setInterval(() => { document.getElementById('tick').textContent = ++n; }, 250);
