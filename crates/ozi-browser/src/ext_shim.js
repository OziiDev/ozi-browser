// Ozi Browser — WebExtension API şimi (Chrome `chrome.*` + Firefox `browser.*`).
// Rust şu yer tutucuları doldurur: __EXTS_JSON__, __NONCE__, __RXNAME__, __HANDOFF__.
// Güvenlik: `rt` hiçbir kalıcı global'de tutulmaz. Rust -> sayfa çağrıları rastgele adlı bir
// işlev + nonce ile yapılır; sayfa -> Rust mesajları nonce taşır. Sayfa betikleri bunları göremez.
(function (NONCE, RXNAME, HANDOFF) {
  if (window[HANDOFF]) return;
  var EXTS = __EXTS_JSON__;
  var stringify = JSON.stringify;
  var ipcObj = window.ipc, ipcPost = ipcObj && ipcObj.postMessage;
  var wk = window.webkit && window.webkit.messageHandlers && window.webkit.messageHandlers.ipc;
  var wkPost = wk && wk.postMessage;
  function post(s) {
    if (wkPost) wkPost.call(wk, s);
    else if (ipcPost) ipcPost.call(ipcObj, s);
  }
  var rt = { exts: EXTS, pending: {}, seq: 0, lastError: null, apis: {}, evL: {} };
  var slice = Array.prototype.slice;

  function send(msg) {
    try { msg.n = NONCE; post(stringify(msg)); } catch (e) {}
  }
  rt.call = function (ext, op, args) {
    return new Promise(function (res, rej) {
      var rid = ++rt.seq;
      rt.pending[rid] = { res: res, rej: rej };
      send({ z: 1, ext: ext, rid: rid, op: op, args: args || [] });
    });
  };
  rt.rx = function (rid, ok, val) {
    var p = rt.pending[rid];
    if (!p) return;
    delete rt.pending[rid];
    if (ok) p.res(val); else p.rej(new Error(String(val)));
  };

  function evs(ext, name) { return (rt.evL[ext + '/' + name] = rt.evL[ext + '/' + name] || []); }
  function ev(list) {
    return {
      addListener: function (f) { if (typeof f === 'function' && list.indexOf(f) < 0) list.push(f); },
      removeListener: function (f) { var i = list.indexOf(f); if (i >= 0) list.splice(i, 1); },
      hasListener: function (f) { return list.indexOf(f) >= 0; },
      hasListeners: function () { return list.length > 0; }
    };
  }
  rt.fire = function (ext, name, args) {
    evs(ext, name).slice().forEach(function (f) {
      try { f.apply(null, args || []); } catch (e) { console.error(e); }
    });
  };
  rt.storageChanged = function (ext, changes, area) {
    rt.fire(ext, 'storage.onChanged', [changes, area]);
    rt.fire(ext, 'storage.' + area + '.onChanged', [changes]);
  };
  // Rust -> sayfa: runtime.sendMessage / tabs.sendMessage teslimi
  rt.dispatch = function (ext, msg, sender, token) {
    var list = evs(ext, 'runtime.onMessage').slice();
    var done = false, async = false;
    function reply(ok, v) {
      if (done) return;
      done = true;
      send({ z: 1, ext: ext, op: '__resp', args: [token, ok, v === undefined ? null : v] });
    }
    for (var i = 0; i < list.length; i++) {
      try {
        var r = list[i](msg, sender, function (v) { reply(true, v); });
        if (r && typeof r.then === 'function') {
          async = true;
          r.then(function (v) { reply(true, v); }, function (e) { reply(false, (e && e.message) || String(e)); });
        } else if (r === true) { async = true; }
      } catch (e) { console.error(e); }
    }
    if (!async) reply(true, undefined);
  };

  // ---- URL eşleştirme (Rust'taki MatchPattern ile aynı kurallar) ----
  function glob(g, t) {
    var re = '^' + g.replace(/[.+^${}()|[\]\\]/g, '\\$&').replace(/\*/g, '[\\s\\S]*').replace(/\?/g, '[\\s\\S]') + '$';
    try { return new RegExp(re).test(t); } catch (e) { return false; }
  }
  rt.match = function (href, ms, ex, ig, eg) {
    var u;
    try { u = new URL(href); } catch (e) { return false; }
    var sc = u.protocol.slice(0, -1);
    function pm(p) {
      if (p === '<all_urls>') return /^(https?|wss?|ftp|file)$/.test(sc);
      var m = /^(\*|[a-z][a-z0-9+.-]*):\/\/([^\/]*)(\/[\s\S]*)$/i.exec(p);
      if (!m) return false;
      if (m[1] === '*') { if (!/^(https?|wss?)$/.test(sc)) return false; }
      else if (m[1].toLowerCase() !== sc) return false;
      var h = m[2].toLowerCase().replace(/:\d+$/, ''), uh = u.hostname.toLowerCase();
      if (h !== '*' && !(sc === 'file' && h === '')) {
        if (h.indexOf('*.') === 0) {
          var s = h.slice(2);
          if (uh !== s && !uh.endsWith('.' + s)) return false;
        } else if (h !== uh) return false;
      }
      return glob(m[3], u.pathname + u.search);
    }
    var any = function (l) { return l.some(pm); };
    if (!any(ms) || any(ex)) return false;
    if (ig.length && !ig.some(function (g) { return glob(g, href); })) return false;
    return !eg.some(function (g) { return glob(g, href); });
  };

  rt.css = function (text) {
    var s = document.createElement('style');
    s.textContent = text;
    (document.head || document.documentElement).appendChild(s);
  };
  function whenRoot(go) {
    if (document.documentElement) return go();
    new MutationObserver(function (_, o) {
      if (document.documentElement) { o.disconnect(); go(); }
    }).observe(document, { childList: true });
  }
  rt.schedule = function (when, fn) {
    var go = function () { try { fn(); } catch (e) { console.error(e); } };
    if (when === 'document_start') whenRoot(go);
    else if (when === 'document_end') {
      if (document.readyState !== 'loading') go();
      else document.addEventListener('DOMContentLoaded', go, { once: true });
    } else {
      if (document.readyState !== 'loading') go();
      else document.addEventListener('DOMContentLoaded', function () { setTimeout(go, 0); }, { once: true });
    }
  };

  // ---- Bilinmeyen API'ler için zararsız taslak: çökmek yerine no-op ----
  function stub() {
    var f = function () {
      var cb = arguments[arguments.length - 1];
      if (typeof cb === 'function') { try { cb(); } catch (e) {} }
      return Promise.resolve();
    };
    return new Proxy(f, {
      get: function (t, k) {
        if (typeof k === 'symbol' || k === 'then') return undefined;
        if (k === 'addListener' || k === 'removeListener' || k === 'removeAllListeners') return function () {};
        if (k === 'hasListener' || k === 'hasListeners') return function () { return false; };
        return stub();
      }
    });
  }

  function cbify(fn) {
    return function () {
      var a = slice.call(arguments), cb = null;
      if (a.length && typeof a[a.length - 1] === 'function') cb = a.pop();
      var p = fn.apply(null, a);
      if (!cb) return p;
      p.then(function (v) { try { cb(v); } catch (e) { console.error(e); } }, function (e) {
        rt.lastError = { message: (e && e.message) || String(e) };
        try { cb(); } finally { rt.lastError = null; }
      });
    };
  }

  rt.mk = function (extId) {
    if (rt.apis[extId]) return rt.apis[extId];
    var E = EXTS[extId];
    if (!E) return null;
    function op(name) {
      return cbify(function () { return rt.call(extId, name, slice.call(arguments)); });
    }
    function area(name) {
      var a = { onChanged: ev(evs(extId, 'storage.' + name + '.onChanged')) };
      ['get', 'set', 'remove', 'clear'].forEach(function (m) { a[m] = op('storage.' + name + '.' + m); });
      a.getBytesInUse = cbify(function () { return Promise.resolve(0); });
      a.setAccessLevel = cbify(function () { return Promise.resolve(); });
      return a;
    }
    function getURL(p) { return E.base + String(p).replace(/^\/+/, ''); }

    var runtime = {
      id: E.pub,
      getURL: getURL,
      getManifest: function () { return JSON.parse(JSON.stringify(E.manifest)); },
      getPlatformInfo: cbify(function () { return Promise.resolve({ os: E.os, arch: 'x86-64', nacl_arch: 'x86-64' }); }),
      getBackgroundPage: cbify(function () { return Promise.resolve(null); }),
      openOptionsPage: op('runtime.openOptionsPage'),
      setUninstallURL: cbify(function () { return Promise.resolve(); }),
      reload: function () {},
      connect: function () {
        return { name: '', postMessage: function () {}, disconnect: function () {},
          onMessage: ev([]), onDisconnect: ev([]) };
      },
      sendMessage: function () {
        var a = slice.call(arguments), cb = null;
        if (a.length && typeof a[a.length - 1] === 'function') cb = a.pop();
        if (a.length > 1 && typeof a[0] === 'string') a.shift(); // (extensionId, message)
        var p = rt.call(extId, 'runtime.sendMessage', [a[0]]);
        if (!cb) return p;
        p.then(function (v) { cb(v); }, function (e) {
          rt.lastError = { message: (e && e.message) || String(e) };
          try { cb(); } finally { rt.lastError = null; }
        });
      },
      onMessage: ev(evs(extId, 'runtime.onMessage')),
      onMessageExternal: ev([]),
      onConnect: ev([]),
      onConnectExternal: ev([]),
      onInstalled: ev(evs(extId, 'runtime.onInstalled')),
      onStartup: ev(evs(extId, 'runtime.onStartup')),
      onSuspend: ev([]),
      OnInstalledReason: { INSTALL: 'install', UPDATE: 'update' }
    };
    Object.defineProperty(runtime, 'lastError', { get: function () { return rt.lastError; } });

    var tabs = {
      TAB_ID_NONE: -1,
      onCreated: ev(evs(extId, 'tabs.onCreated')),
      onUpdated: ev(evs(extId, 'tabs.onUpdated')),
      onActivated: ev(evs(extId, 'tabs.onActivated')),
      onRemoved: ev(evs(extId, 'tabs.onRemoved'))
    };
    ['query', 'get', 'getCurrent', 'create', 'update', 'remove', 'reload', 'sendMessage', 'executeScript', 'insertCSS']
      .forEach(function (m) { tabs[m] = op('tabs.' + m); });

    var action = {
      onClicked: ev(evs(extId, 'action.onClicked')),
      setBadgeText: cbify(function () { return Promise.resolve(); }),
      setBadgeBackgroundColor: cbify(function () { return Promise.resolve(); }),
      setTitle: cbify(function () { return Promise.resolve(); }),
      setIcon: cbify(function () { return Promise.resolve(); }),
      setPopup: cbify(function () { return Promise.resolve(); }),
      getBadgeText: cbify(function () { return Promise.resolve(''); }),
      enable: cbify(function () { return Promise.resolve(); }),
      disable: cbify(function () { return Promise.resolve(); })
    };

    var timers = {};
    var alarms = {
      onAlarm: ev(evs(extId, 'alarms.onAlarm')),
      create: function (name, info) {
        if (typeof name === 'object') { info = name; name = ''; }
        info = info || {};
        alarms.clear(name);
        var fire = function () { rt.fire(extId, 'alarms.onAlarm', [{ name: name, scheduledTime: Date.now() }]); };
        var first = info.delayInMinutes != null ? info.delayInMinutes * 60000
          : info.when ? info.when - Date.now() : (info.periodInMinutes || 0) * 60000;
        timers[name] = setTimeout(function () {
          fire();
          if (info.periodInMinutes) timers[name] = setInterval(fire, Math.max(info.periodInMinutes, 0.1) * 60000);
        }, Math.max(first, 0));
      },
      clear: cbify(function (name) {
        clearTimeout(timers[name || '']); clearInterval(timers[name || '']);
        return Promise.resolve(true);
      }),
      clearAll: cbify(function () {
        Object.keys(timers).forEach(function (k) { clearTimeout(timers[k]); clearInterval(timers[k]); });
        return Promise.resolve(true);
      }),
      get: cbify(function () { return Promise.resolve(undefined); }),
      getAll: cbify(function () { return Promise.resolve([]); })
    };

    var ph = function (m) {
      var out = {};
      Object.keys(m.placeholders || {}).forEach(function (k) { out[k.toLowerCase()] = m.placeholders[k]; });
      return out;
    };
    var i18n = {
      getMessage: function (key, subs) {
        var m = E.messages[String(key).toLowerCase()];
        if (!m) return '';
        var arr = subs == null ? [] : Array.isArray(subs) ? subs : [subs], lph = ph(m);
        var sub = function (s) {
          return s.replace(/\$([1-9])/g, function (_, d) { var v = arr[d - 1]; return v == null ? '' : String(v); });
        };
        return String(m.message || '').replace(/\$\$|\$([a-z0-9_@]+)\$|\$([1-9])/gi, function (all, name, d) {
          if (all === '$$') return '$';
          if (name) { var c = lph[name.toLowerCase()]; return sub(c && c.content != null ? String(c.content) : ''); }
          var v = arr[d - 1];
          return v == null ? '' : String(v);
        });
      },
      getUILanguage: function () { return navigator.language || 'en'; },
      getAcceptLanguages: cbify(function () { return Promise.resolve([navigator.language || 'en']); }),
      detectLanguage: cbify(function () { return Promise.resolve({ isReliable: false, languages: [] }); })
    };

    var scripting = {
      executeScript: cbify(function (inj) {
        var o = { target: inj.target || {}, files: inj.files || null, code: null };
        if (inj.func) o.code = '(' + inj.func.toString() + ').apply(null,' + JSON.stringify(inj.args || []) + ')';
        return rt.call(extId, 'scripting.executeScript', [o]);
      }),
      insertCSS: op('scripting.insertCSS'),
      removeCSS: cbify(function () { return Promise.resolve(); })
    };

    var base = {
      runtime: runtime,
      extension: { getURL: getURL, inIncognitoContext: false, getBackgroundPage: function () { return null; } },
      storage: {
        local: area('local'), sync: area('sync'), session: area('session'), managed: area('managed'),
        onChanged: ev(evs(extId, 'storage.onChanged'))
      },
      tabs: tabs,
      i18n: i18n,
      alarms: alarms,
      scripting: scripting,
      action: action,
      browserAction: action,
      pageAction: action,
      permissions: {
        contains: cbify(function () { return Promise.resolve(true); }),
        request: cbify(function () { return Promise.resolve(true); }),
        getAll: cbify(function () { return Promise.resolve({ permissions: [], origins: [] }); }),
        onAdded: ev([]), onRemoved: ev([])
      },
      management: { getSelf: cbify(function () { return Promise.resolve({ id: E.pub, name: E.manifest.name, version: E.manifest.version, enabled: true }); }) },
      windows: {
        WINDOW_ID_CURRENT: -2, WINDOW_ID_NONE: -1,
        getCurrent: cbify(function () { return Promise.resolve({ id: 1, focused: true, type: 'normal' }); }),
        getAll: cbify(function () { return Promise.resolve([{ id: 1, focused: true, type: 'normal' }]); })
      }
    };
    var api = new Proxy(base, {
      get: function (t, k) {
        if (k in t) return t[k];
        return typeof k === 'string' ? (t[k] = stub()) : undefined; // webRequest, contextMenus, ...
      }
    });
    rt.apis[extId] = api;
    return api;
  };

  // Rust -> sayfa: tek giriş noktası (nonce doğrulamalı)
  var rx = function (n, kind, a, b, c, d) {
    if (n !== NONCE) return;
    if (kind === 'rx') rt.rx(a, b, c);
    else if (kind === 'dispatch') rt.dispatch(a, b, c, d);
    else if (kind === 'storage') rt.storageChanged(a, b, c);
    else if (kind === 'fire') rt.fire(a, b, c);
  };
  Object.defineProperty(window, RXNAME, { value: rx, enumerable: false, configurable: true });
  // Aynı başlangıç aşamasında çalışan içerik betikleri için tek seferlik devir; sonra silinir.
  Object.defineProperty(window, HANDOFF, { value: rt, enumerable: false, configurable: true });
})(__NONCE__, __RXNAME__, __HANDOFF__);
