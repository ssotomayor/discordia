return (async function () {
  if (window.__dxfTextMenuOff) window.__dxfTextMenuOff();
  let menu = null, context = null, waiting = null, alive = true;
  const listeners = [];
  let stop;
  const stopped = new Promise(resolve => { stop = resolve; });
  const on = (target, name, fn, capture = false) => {
    target.addEventListener(name, fn, capture);
    listeners.push(() => target.removeEventListener(name, fn, capture));
  };
  function close(restoreFocus = false) {
    if (menu) menu.remove();
    menu = null;
    if (restoreFocus && context && context.target.isConnected) context.target.focus();
  }
  function editable(target) {
    const el = target.closest('input, textarea, [contenteditable="true"]');
    if (!el || el.disabled) return null;
    if (el.tagName === 'INPUT' && !['text', 'search', 'url', 'tel', 'email', 'password'].includes(el.type)) return null;
    return el;
  }
  function selectedText(target, field) {
    if (field && field.tagName === 'INPUT' && field.type === 'password') return '';
    if (field && typeof field.selectionStart === 'number') return field.value.slice(field.selectionStart, field.selectionEnd);
    const selection = window.getSelection();
    if (!selection || !selection.rangeCount || selection.isCollapsed) return '';
    const range = selection.getRangeAt(0);
    if (!range.intersectsNode(target)) return '';
    function text(node) {
      if (node.nodeType === 3) return node.nodeValue;
      if (node.nodeName === 'IMG') return node.alt || '';
      if (node.nodeName === 'BR') return '\n';
      const body = Array.from(node.childNodes || [], text).join('');
      return ['DIV', 'P', 'LI'].includes(node.nodeName) ? body + '\n' : body;
    }
    return text(range.cloneContents());
  }
  function restore(c) {
    if (!c.target.isConnected) return false;
    c.target.focus();
    if (c.start !== null) c.target.setSelectionRange(c.start, c.end);
    else if (c.range && c.range.startContainer.isConnected && c.range.endContainer.isConnected) {
      const selection = window.getSelection();
      selection.removeAllRanges();
      selection.addRange(c.range);
    }
    return true;
  }
  function request(action, text) {
    return new Promise(resolve => { waiting = resolve; dioxus.send({ action, text }); });
  }
  function change(c, text, cut = false) {
    text = text.replace(/\r\n?/g, '\n');
    if (typeof c.target.selectionStart === 'number') {
      c.target.setRangeText(text, c.start, c.end, 'end');
      c.target.dispatchEvent(new Event('input', { bubbles: true }));
    } else {
      document.execCommand(cut ? 'delete' : 'insertText', false, text);
      c.target.dispatchEvent(new Event('input', { bubbles: true }));
    }
  }
  async function act(action, c) {
    if (waiting) return;
    if (!c.target.isConnected) { close(); return; }
    if (action === 'all') {
      if (!restore(c)) { close(); return; }
      if (typeof c.target.select === 'function') c.target.select();
      else {
        const range = document.createRange(); range.selectNodeContents(c.target);
        const selection = window.getSelection(); selection.removeAllRanges(); selection.addRange(range);
      }
      close(); return;
    }
    const result = await request(action === 'paste' ? 'paste' : 'copy', action === 'message' ? c.message : c.text);
    // A slow clipboard response must not edit a different field or a replacement composer.
    if (!alive || context !== c || !c.target.isConnected) return;
    if (!result.ok || (result.image && (!c.field || !c.target.closest('#dxf-chat-drop') || !window.__dxfPasteSink))) {
      if (menu) {
        let status = menu.querySelector('[role="status"]');
        if (!status) { status = document.createElement('div'); status.setAttribute('role', 'status'); menu.append(status); }
        status.textContent = window.__dxfTextMenuLabels.error;
      }
      return;
    }
    if (action === 'cut' || action === 'paste') {
      if (!restore(c)) { close(); return; }
    }
    if (action === 'cut') change(c, '', true);
    else if (action === 'paste') {
      if (result.image) window.__dxfPasteSink();
      else change(c, result.text);
    }
    close(true);
  }
  // The release build cancels native menus on document before later listeners run.
  on(document.body, 'contextmenu', e => {
    close(); context = null;
    if (e.defaultPrevented || waiting || !e.target.closest) return;
    const field = editable(e.target);
    if (!field && e.target.closest('button, [role="button"]')) return;
    const row = e.target.closest('[data-context-message]');
    const target = field || row || e.target;
    const text = selectedText(target, field);
    const message = row ? row.getAttribute('data-context-message') : '';
    const actions = !field && row ? Array.from(row.querySelectorAll('[data-context-action]')).filter(button => button.dataset.contextAction && !button.disabled) : [];
    if (!field && !text && !message && !actions.length) return;
    e.preventDefault();
    const selection = window.getSelection();
    const c = { target, field, text, message,
      start: field && typeof field.selectionStart === 'number' ? field.selectionStart : null,
      end: field && typeof field.selectionEnd === 'number' ? field.selectionEnd : null,
      range: selection && selection.rangeCount ? selection.getRangeAt(0).cloneRange() : null };
    context = c;
    menu = document.createElement('div');
    menu.className = 'dxf-text-menu'; menu.setAttribute('role', 'menu');
    const labels = window.__dxfTextMenuLabels;
    const theme = getComputedStyle(target);
    for (const name of ['--panel-solid', '--border', '--text', '--accent', '--danger']) menu.style.setProperty(name, theme.getPropertyValue(name));
    menu.style.fontFamily = theme.fontFamily;
    const modifier = /Mac/.test(navigator.platform) ? '⌘' : 'Ctrl+';
    function item(action, shortcut) {
      const button = document.createElement('button');
      button.type = 'button'; button.setAttribute('role', 'menuitem');
      const label = document.createElement('span'); label.textContent = labels[action]; button.append(label);
      if (shortcut) { const kbd = document.createElement('kbd'); kbd.textContent = modifier + shortcut; button.append(kbd); }
      button.addEventListener('mousedown', event => event.preventDefault());
      button.addEventListener('click', () => { act(action, c).catch(() => close()); });
      menu.append(button);
    }
    function messageAction(source, parent = menu, emoji = false) {
      const button = document.createElement('button');
      button.type = 'button'; button.setAttribute('role', 'menuitem');
      button.textContent = emoji ? source.dataset.contextEmoji : source.title;
      button.title = emoji ? labels.react + ' ' + source.dataset.contextEmoji : source.title;
      if (source.dataset.contextAction === 'delete') button.className = 'dxf-text-menu-danger';
      button.addEventListener('click', () => {
        close(); context = null;
        if (source.isConnected && !source.disabled) source.click();
      });
      parent.append(button);
    }
    function separator() {
      const line = document.createElement('div'); line.className = 'dxf-text-menu-separator';
      line.setAttribute('role', 'separator'); menu.append(line);
    }
    if (actions.length) {
      const quick = Array.from(row.querySelectorAll('[data-context-emoji]'));
      if (quick.length) {
        const bar = document.createElement('div'); bar.className = 'dxf-text-menu-reactions';
        bar.setAttribute('role', 'group'); bar.setAttribute('aria-label', labels.react);
        for (const button of quick) messageAction(button, bar, true);
        menu.append(bar);
      }
      const primary = actions.filter(button => ['react', 'reply'].includes(button.dataset.contextAction));
      for (const button of primary) messageAction(button);
      if (quick.length || primary.length) separator();
    }
    if (text) item('copy', 'C');
    if (message) item('message');
    for (const button of actions.filter(button => ['bookmark', 'pin', 'edit'].includes(button.dataset.contextAction))) messageAction(button);
    const remove = actions.find(button => button.dataset.contextAction === 'delete');
    if (remove) { separator(); messageAction(remove); }
    if (field) {
      if (!field.readOnly) {
        if (text) item('cut', 'X');
        item('paste', 'V');
      }
      item('all', 'A');
    }
    document.body.append(menu);
    const bounds = menu.getBoundingClientRect();
    menu.style.left = Math.max(8, Math.min(e.clientX, innerWidth - bounds.width - 8)) + 'px';
    menu.style.top = Math.max(8, Math.min(e.clientY, innerHeight - bounds.height - 8)) + 'px';
    menu.querySelector('button').focus({ preventScroll: true });
  });
  on(document, 'pointerdown', e => { if (menu && !menu.contains(e.target)) { close(); context = null; } }, true);
  on(document, 'scroll', e => {
    if (menu && menu.contains(e.target)) return;
    close(); context = null;
  }, true);
  on(window, 'resize', () => { close(); context = null; });
  on(window, 'blur', () => { close(); context = null; });
  on(document, 'keydown', e => {
    if (!menu) return;
    if (e.key === 'Escape' || e.key === 'Tab') { e.preventDefault(); close(true); context = null; }
    else if (['ArrowDown', 'ArrowUp', 'Home', 'End'].includes(e.key)) {
      e.preventDefault();
      const buttons = Array.from(menu.querySelectorAll('button'));
      let i = buttons.indexOf(document.activeElement);
      i = e.key === 'Home' ? 0 : e.key === 'End' ? buttons.length - 1 : (i + (e.key === 'ArrowDown' ? 1 : -1) + buttons.length) % buttons.length;
      buttons[i].focus({ preventScroll: true });
    }
  }, true);
  const off = () => { alive = false; close(); context = null; listeners.forEach(remove => remove()); stop(); };
  window.__dxfTextMenuOff = off;
  try {
    while (alive) {
      const result = await Promise.race([dioxus.recv(), stopped]);
      if (!alive) break;
      if (waiting) { const resolve = waiting; waiting = null; resolve(result); }
    }
  } finally {
    off();
    if (window.__dxfTextMenuOff === off) delete window.__dxfTextMenuOff;
  }
})();
