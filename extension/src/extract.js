// @ts-check
// The in-page extractor. It is serialised and injected (chrome.scripting in a tab,
// Runtime.evaluate in the native host's browser), so it must stay self-contained:
// no imports, no outside variables. It never throws; problems come back as `error`.
// Everything it reads is untrusted and is validated again by the Rust core.

/**
 * @param {{ fields: { name: string, selector: string, kind?: string }[], rowSelector?: string }} args
 * @returns {{ url: string, status: number, output?: { pageUrl: string, rows: { pageUrl: string, fields: Record<string, string> }[], warnings: string[], emptyFields: { name: string, selector: string }[] }, error?: string, challenge?: boolean }}
 */
export function extractFields(args) {
  const url = location.href;
  let status = 0;
  try {
    const nav = /** @type {any} */ (performance.getEntriesByType('navigation')[0]);
    status = Number(nav?.responseStatus) || 0;
  } catch { /* status stays unknown */ }
  const title = (document.title || '').trim();
  // Only read the page text when the title already looks like a verification page.
  if (/^(just a moment|attention required)/i.test(title) &&
    /cloudflare|verify you are human|verifies you are not a bot|security verification/i.test(document.body?.innerText || '')) {
    return { url, status, error: 'Browser verification is showing', challenge: true };
  }
  try {
    const fields = args.fields;
    const rowSelector = args.rowSelector || '';
    /** @type {string[]} */
    const warnings = [];
    /** @param {Element} el @param {string | undefined} kind */
    const read = (el, kind) => {
      if (kind === 'href') return /** @type {HTMLAnchorElement} */ (el).href || el.getAttribute('href') || '';
      if (kind === 'html') return el.innerHTML.trim();
      if (kind === 'value') return String(/** @type {HTMLInputElement} */ (el).value ?? el.getAttribute('value') ?? '');
      return (/** @type {HTMLElement} */ (el).innerText || el.textContent || '').trim();
    };
    /** @param {ParentNode} root @param {string} selector @param {string} name */
    const query = (root, selector, name) => {
      try { return [...root.querySelectorAll(selector)]; }
      catch { throw new Error(`Invalid selector for ${name}. Pick that field again.`); }
    };
    const roots = rowSelector ? query(document, rowSelector, 'record container') : [document];
    if (!roots.length) throw new Error('No record containers matched. Pick the record container again.');
    if (roots.length > 5000) throw new Error('More than 5000 records. Select a smaller page section.');
    /** @type {{ pageUrl: string, fields: Record<string, string> }[]} */
    const rows = [];
    for (const [index, root] of roots.entries()) {
      const columns = fields.map(field => query(root, field.selector, field.name).map(el => read(el, field.kind)));
      if (rowSelector) {
        // Several matches inside one record belong to it (e.g. a list of tags).
        const values = Object.fromEntries(fields.map((field, i) => [field.name, columns[i].join('\n')]));
        for (let i = 0; i < fields.length; i++) {
          if (!columns[i].length && warnings.length < 30) warnings.push(`Record ${index + 1}: no matches for ${fields[i].name}`);
        }
        if (Object.values(values).some(Boolean)) rows.push({ pageUrl: url, fields: values });
      } else {
        const counts = columns.map(c => c.length);
        if (new Set(counts).size > 1) throw new Error('Field match counts differ. Pick a record container to avoid pairing unrelated values.');
        const count = counts[0] || 0;
        if (count > 5000) throw new Error('More than 5000 matches. Select a smaller page section.');
        if (count > 1 && fields.length > 1 && warnings.length < 30) warnings.push('Positional extraction: equal counts do not guarantee related fields. Pick a record container.');
        for (let i = 0; i < count; i++) {
          const values = Object.fromEntries(fields.map((field, j) => [field.name, columns[j][i]]));
          if (Object.values(values).some(Boolean)) rows.push({ pageUrl: url, fields: values });
        }
      }
    }
    /** @type {{ name: string, selector: string }[]} */
    const emptyFields = [];
    if (rows.length && fields.length > 1) {
      for (const field of fields) {
        if (rows.every(row => !row.fields[field.name])) emptyFields.push({ name: field.name, selector: field.selector });
      }
    }
    return { url, status, output: { pageUrl: url, rows, warnings, emptyFields } };
  } catch (error) {
    return { url, status, error: String(/** @type {Error} */ (error)?.message || error), challenge: false };
  }
}
