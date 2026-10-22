// For a resource's UI page: <script src="ivmp://ui/nui.js"></script> (the web layer also puts it
// into every resource page it serves, so FiveM pages work without it).
//
//   ivmpNui.post('name', data)   calls RegisterNUICallback('name', function(data, cb) ... end) in the
//                                resource's client script; the promise gets what cb(result) passes.
//   window.addEventListener('message', (e) => e.data)   what the script sends with SendNUIMessage.
//
// FiveM pages call their callbacks as POSTs to http://<resource>/<name> (XMLHttpRequest or fetch)
// and use GetParentResourceName(); both work here: such a request becomes ivmpNui.post(name, body)
// and its response is what cb(result) passed (as JSON, a string as it is). Nothing leaves the page.
if (!window.ivmpNui) window.ivmpNui = (() => {
  let next = 0;
  const waiting = {};
  window.addEventListener('message', (event) => {
    const message = event.data;
    if (message && message.ivmpReply === true && waiting[message.id]) {
      waiting[message.id](message.result);
      delete waiting[message.id];
    }
  });
  const nui = {
    post(name, data) {
      const id = ++next;
      return new Promise((resolve) => {
        waiting[id] = resolve;
        window.parent.postMessage({ ivmpCallback: true, name, data, id }, '*');
      });
    },
  };

  // This page's resource: the host of its ivmp://<resource>/ address.
  window.GetParentResourceName = () => location.hostname;

  // A FiveM callback address: an http(s) URL; its path is the callback name.
  const callbackOf = (url) => {
    let parsed;
    try { parsed = new URL(url, location.href); } catch (e) { return null; }
    if (parsed.protocol !== 'http:' && parsed.protocol !== 'https:') return null;
    return decodeURIComponent(parsed.pathname.replace(/^\/+/, ''));
  };
  const bodyOf = (body) => {
    if (typeof body !== 'string') return body ?? null;
    try { return JSON.parse(body); } catch (e) { return body; }
  };
  const textOf = (result) => (typeof result === 'string' ? result : JSON.stringify(result ?? null));

  const realFetch = window.fetch.bind(window);
  window.fetch = (input, init) => {
    const name = callbackOf(typeof input === 'string' ? input : input && input.url);
    if (name === null) return realFetch(input, init);
    return nui.post(name, bodyOf(init && init.body)).then((result) =>
      new Response(textOf(result), { status: 200, headers: { 'Content-Type': 'application/json' } }));
  };

  const xhr = XMLHttpRequest.prototype;
  const realOpen = xhr.open, realSend = xhr.send, realHeader = xhr.setRequestHeader;
  xhr.open = function (method, url, ...rest) {
    this.__ivmpCallback = callbackOf(url);
    if (this.__ivmpCallback === null) return realOpen.call(this, method, url, ...rest);
  };
  xhr.setRequestHeader = function (...args) {
    if (this.__ivmpCallback == null) return realHeader.apply(this, args);
  };
  xhr.send = function (body) {
    if (this.__ivmpCallback == null) return realSend.call(this, body);
    nui.post(this.__ivmpCallback, bodyOf(body)).then((result) => {
      const text = textOf(result);
      for (const [key, value] of [['readyState', 4], ['status', 200], ['statusText', 'OK'], ['responseText', text], ['response', text]]) {
        Object.defineProperty(this, key, { value, configurable: true });
      }
      // Also runs onload / onreadystatechange (they are listeners of a real XMLHttpRequest).
      for (const type of ['readystatechange', 'load', 'loadend']) this.dispatchEvent(new Event(type));
    });
  };
  return nui;
})();
