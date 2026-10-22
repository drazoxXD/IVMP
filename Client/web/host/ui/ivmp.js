// The page's one bridge to the game: ivmp.send(name, data) goes to the client script runner,
// ivmp.on(name, fn) receives what the client emits. There is no other way out of the page.
(() => {
  const handlers = {};
  window.ivmp = {
    send(name, data) {
      return new Promise((resolve, reject) => window.cefQuery({
        request: JSON.stringify({ name, data }),
        onSuccess: resolve,
        onFailure: (code, message) => reject(new Error(message)),
      }));
    },
    on(name, fn) { (handlers[name] ||= []).push(fn); },
    __emit(name, data) { (handlers[name] || []).forEach((fn) => fn(data)); },
  };
})();
