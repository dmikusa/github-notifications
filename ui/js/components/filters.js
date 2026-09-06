// js/components/filters.js:
// Remembers each view's filter selections per workspace, so they survive tab
// switches, workspace switches, and reloads (bulk actions, sync, dismiss).
// Filters are scoped to (workspace, view) because e.g. the repo-set dropdown
// offers different choices per workspace. The server re-renders the view with
// the filters applied, so capturing the rendered filter form after each swap
// keeps this store in sync.
window.App = window.App || {};

window.App.filters = (() => {
  const saved = {};

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

  return { capture, forView };
})();