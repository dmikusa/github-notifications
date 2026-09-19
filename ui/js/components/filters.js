// js/components/filters.js:
// Remembers each view's filter selections per workspace, so they survive tab
// switches, workspace switches, and reloads (bulk actions, sync, dismiss).
// Filters are scoped to (workspace, view) because e.g. the repo-set dropdown
// offers different choices per workspace. The server re-renders the view with
// the filters applied, so capturing the rendered filter form after each swap
// keeps this store in sync. Also wires the multi-select author popovers.
window.App = window.App || {};

window.App.filters = (() => {
  const saved = {};
  let authorPopoverOpen = false;

  // Read the current filter form (if any) and remember it for (ws, view).
  function capture(view) {
    if (!view) return;
    const form = document.querySelector('#view form.filters');
    if (!form || !form.querySelector('select[name]')) return;
    const params = {};
    form.querySelectorAll('select[name], input[name]').forEach((el) => {
      if (el.type === 'checkbox') {
        if (el.checked) params[el.name] = el.value;
      } else if (el.name && el.name !== 'ws') {
        params[el.name] = el.value;
      }
    });
    // The author filter lives in a hidden input (comma-separated), set by the
    // popover checkboxes below; it's already captured as a scalar above.
    // Key by the workspace the form was actually rendered for (its hidden ws
    // input), not the in-flight state — the workspace may have just changed.
    const ws = form.querySelector('input[name="ws"]')?.value || '';
    saved[`${ws}/${view}`] = params;
  }

  // The saved filter params for the current workspace and `view`.
  function forView(view) {
    const stored = saved[`${window.App.state.currentWorkspace}/${view}`];
    return stored ? { ...stored } : {};
  }

  // Wire the multi-select author popover in a freshly rendered view.
  function bindPopover(root) {
    if (!root) return;
    root.querySelectorAll('[data-popover="author"]').forEach((wrap) => {
      const btn = wrap.querySelector('[data-toggle]');
      const panel = wrap.querySelector('.multi-select-pop');
      if (!btn || !panel) return;
      btn.addEventListener('click', (e) => {
        e.stopPropagation();
        authorPopoverOpen = panel.hidden;
        panel.hidden = !panel.hidden;
      });
      // Selecting an author updates the hidden input first, then the change
      // bubbles to the popover container's hx-get so the reload carries the
      // full selection. Keep the popover open across the swap so multiple
      // authors can be picked at once.
      wrap.querySelectorAll('input.author-opt').forEach((cb) => {
        cb.addEventListener('change', () => {
          syncAuthorHidden(wrap);
          updateAuthorLabel(wrap);
          authorPopoverOpen = true;
        });
      });
      // Close when clicking elsewhere.
      document.addEventListener('click', (e) => {
        if (!panel.hidden && !e.target.closest('[data-popover="author"]')) {
          panel.hidden = true;
          authorPopoverOpen = false;
        }
      });
      syncAuthorHidden(wrap);
      updateAuthorLabel(wrap);
    });
  }

  function syncAuthorHidden(wrap) {
    const hidden = wrap.closest('form').querySelector('input[name="author"]');
    if (!hidden) return;
    hidden.value = [...wrap.querySelectorAll('input.author-opt:checked')]
      .map((c) => c.value)
      .join(',');
  }

  function updateAuthorLabel(wrap) {
    const btn = wrap.querySelector('[data-toggle]');
    if (!btn) return;
    const count = wrap.querySelectorAll('input.author-opt:checked').length;
    btn.textContent = count ? `Author (${count}) \u25be` : 'Author \u25be';
  }

  // Called after the view is swapped in: re-wire the popover and keep it open
  // if the user was selecting authors.
  function afterSwap() {
    const root = document.querySelector('#view');
    bindPopover(root);
    if (root && authorPopoverOpen) {
      root.querySelectorAll('[data-popover="author"]').forEach((wrap) => {
        const panel = wrap.querySelector('.multi-select-pop');
        if (panel) panel.hidden = false;
      });
    }
  }

  return { capture, forView, afterSwap };
})();