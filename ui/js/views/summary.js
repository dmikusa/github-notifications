// js/views/summary.js:
// Summary dashboard: mentions, CI failures, recent discussions, and review
// requests. Mention and CI rows are selectable; the bulk bar marks them read
// or dismisses them.
window.App = window.App || {};

window.App.views = window.App.views || {};

window.App.views.summary = (() => {
  function boxes() {
    return Array.from(document.querySelectorAll('#view .summary-sel'));
  }

  function selected() {
    return boxes().filter((cb) => cb.checked);
  }

  // Reflect the current selection in the bulk bar and each section's
  // select-all checkbox.
  function update() {
    const sel = selected();
    const count = document.querySelector('#view .summary-sel-count');
    if (count) count.textContent = sel.length ? `${sel.length} selected` : '';
    document.querySelectorAll('#view [data-summary-action]').forEach((btn) => {
      btn.disabled = sel.length === 0;
    });
    document.querySelectorAll('#view .summary-sel-all').forEach((all) => {
      const table = all.closest('table');
      const rows = table ? Array.from(table.querySelectorAll('.summary-sel')) : [];
      const checked = rows.filter((cb) => cb.checked).length;
      all.checked = rows.length > 0 && checked === rows.length;
      all.indeterminate = checked > 0 && checked < rows.length;
    });
  }

  async function run(action) {
    const sel = selected();
    if (!sel.length) return;
    const ids = sel.map((cb) => cb.value);
    if (action === 'open') {
      sel
        .map((cb) => cb.dataset.htmlUrl)
        .filter(Boolean)
        .forEach((url) => window.open(url, '_blank', 'noopener'));
      return;
    }
    try {
      if (action === 'mark-read') {
        await window.App.api.postJSON('/api/threads/mark-read', { ids });
      } else if (action === 'dismiss') {
        await window.App.api.postJSON('/api/threads/mute', { ids });
      } else {
        return;
      }
      const verb = action === 'dismiss' ? 'Dismissed' : 'Marked';
      window.App.flash.show(`${verb} ${ids.length}`);
      await window.App.views.reload();
    } catch (err) {
      window.App.flash.show(`Action failed: ${err.message}`, true);
    }
  }

  // Delegated so the handlers survive view swaps and incremental repaints.
  document.addEventListener('change', (e) => {
    const target = e.target;
    if (!(target instanceof Element)) return;
    if (target.matches('#view .summary-sel-all')) {
      const table = target.closest('table');
      if (table) {
        table.querySelectorAll('.summary-sel').forEach((cb) => {
          cb.checked = target.checked;
        });
      }
      update();
    } else if (target.matches('#view .summary-sel')) {
      update();
    }
  });

  document.addEventListener('click', (e) => {
    const target = e.target;
    if (!(target instanceof Element)) return;
    const btn = target.closest('#view [data-summary-action]');
    if (btn) run(btn.dataset.summaryAction);
  });

  return { update };
})();
