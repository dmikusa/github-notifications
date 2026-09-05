// js/views/settings.js:
// Repo set management: remove repo sets/repos, and an org browser that
// checkbox-selects repos and saves them into a repo set (config write-back).
window.App = window.App || {};

window.App.settings = (() => {
  let org = '';
  let page = 1;
  let hasMore = false;
  let selected = new Set();

  function el(id) {
    return document.getElementById(id);
  }

  async function reload() {
    await window.App.views.reload();
  }

  function renderRepos(repos) {
    const list = el('org-repo-list');
    for (const repo of repos) {
      const label = document.createElement('label');
      const cb = document.createElement('input');
      cb.type = 'checkbox';
      cb.className = 'org-repo';
      cb.value = repo.full_name;
      if (selected.has(repo.full_name)) cb.checked = true;
      cb.addEventListener('change', () => {
        if (cb.checked) {
          selected.add(repo.full_name);
        } else {
          selected.delete(repo.full_name);
        }
      });
      label.appendChild(cb);
      label.appendChild(document.createTextNode(` ${repo.full_name}`));
      list.appendChild(label);
    }
  }

  function filterRepos() {
    const q = (el('org-filter').value || '').toLowerCase();
    document.querySelectorAll('#org-repo-list label').forEach((label) => {
      const name = label.textContent.trim().toLowerCase();
      label.hidden = !!q && !name.includes(q);
    });
  }

  async function loadPage() {
    const params = new URLSearchParams({ page: String(page), q: '' });
    const data = await window.App.api.getJSON(
      `/api/orgs/${encodeURIComponent(org)}/repos?${params}`
    );
    hasMore = data.has_more;
    renderRepos(data.repos);
    el('load-more').hidden = !hasMore;
    filterRepos();
  }

  function bind() {
    const root = document.querySelector('#view');
    if (!root) return;

    root.querySelectorAll('[data-action="remove-set"]').forEach((btn) => {
      btn.addEventListener('click', async () => {
        const ws = window.App.state.currentWorkspace;
        await window.App.api.deleteJSON(
          `/api/workspaces/${encodeURIComponent(ws)}/repo-sets/${encodeURIComponent(btn.dataset.set)}`
        );
        reload();
      });
    });

    root.querySelectorAll('[data-action="remove-repo"]').forEach((btn) => {
      btn.addEventListener('click', async () => {
        const ws = window.App.state.currentWorkspace;
        await window.App.api.deleteJSON(
          `/api/workspaces/${encodeURIComponent(ws)}/repo-sets/${encodeURIComponent(btn.dataset.set)}/repos/${encodeURIComponent(btn.dataset.repo)}`
        );
        reload();
      });
    });

    const form = el('org-picker');
    if (!form) return;
    form.addEventListener('submit', async (e) => {
      e.preventDefault();
      const setname = form.elements['set-name'].value.trim();
      org = form.elements.org.value.trim();
      if (!setname || !org) return;
      el('org-repos').hidden = false;
      el('org-repo-list').innerHTML = '';
      selected = new Set();
      page = 1;
      hasMore = false;
      el('load-more').hidden = true;
      await loadPage();
    });

    el('load-more').addEventListener('click', async () => {
      page += 1;
      await loadPage();
    });

    el('org-filter').addEventListener('input', filterRepos);

    el('save-set').addEventListener('click', async () => {
      const setname = form.elements['set-name'].value.trim();
      const ws = window.App.state.currentWorkspace;
      await window.App.api.postJSON(
        `/api/workspaces/${encodeURIComponent(ws)}/repo-sets`,
        { name: setname, repos: Array.from(selected) }
      );
      selected = new Set();
      await reload();
    });
  }

  return { bind };
})();