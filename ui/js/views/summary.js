// js/views/summary.js:
// Summary dashboard: mentions, CI failures, recent discussions, and review
// requests. Each section selects and acts only on its own rows.
window.App = window.App || {};

window.App.views = window.App.views || {};

window.App.views.summary = (() => {
  function sectionRoot(section) {
    return document.querySelector(`#view .summary-section[data-section="${section}"]`);
  }

  function boxes(section) {
    const root = sectionRoot(section);
    return root ? Array.from(root.querySelectorAll('.summary-sel')) : [];
  }

  function selected(section) {
    return boxes(section).filter((cb) => cb.checked);
  }

  // Reflect the section's selection in its count, action buttons, and
  // select-all checkbox.
  function updateSection(section) {
    const root = sectionRoot(section);
    if (!root) return;
    const sel = selected(section);
    const count = root.querySelector(`[data-sel-count="${section}"]`);
    if (count) count.textContent = sel.length ? `${sel.length} selected` : '';
    root.querySelectorAll('[data-summary-action]').forEach((btn) => {
      btn.disabled = sel.length === 0;
    });
    const all = root.querySelector('.summary-sel-all');
    if (all) {
      const rows = boxes(section);
      const checked = rows.filter((cb) => cb.checked).length;
      all.checked = rows.length > 0 && checked === rows.length;
      all.indeterminate = checked > 0 && checked < rows.length;
    }
  }

  function updateAll() {
    document.querySelectorAll('#view .summary-section').forEach((section) => {
      updateSection(section.dataset.section);
    });
  }

  async function run(action, section) {
    const sel = selected(section);
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
      const section = target.dataset.section;
      const root = sectionRoot(section);
      if (root) {
        root.querySelectorAll('.summary-sel').forEach((cb) => {
          cb.checked = target.checked;
        });
      }
      updateSection(section);
    } else if (target.matches('#view .summary-sel')) {
      const section = target.closest('.summary-section')?.dataset.section;
      if (section) updateSection(section);
    }
  });

  document.addEventListener('click', (e) => {
    const target = e.target;
    if (!(target instanceof Element)) return;
    const btn = target.closest('#view [data-summary-action]');
    if (btn) run(btn.dataset.summaryAction, btn.dataset.section);
  });

  return { update: updateAll };
})();
