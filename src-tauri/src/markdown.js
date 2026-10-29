// Converts the page's main content to Markdown. Evaluated in the page by crawler.rs;
// __INCLUDE_IMAGES__ is substituted with true/false. Returns JSON: {"md": "..."}.
(function() {
    const INCLUDE_IMAGES = __INCLUDE_IMAGES__;
    const SKIP = new Set(['SCRIPT', 'STYLE', 'NOSCRIPT', 'TEMPLATE', 'IFRAME', 'SVG', 'CANVAS', 'BUTTON',
                          'INPUT', 'SELECT', 'TEXTAREA', 'OPTION', 'OBJECT', 'EMBED', 'AUDIO', 'VIDEO']);
    const root = document.querySelector('main, [role="main"], #main, #content, article') || document.body;

    // Deliberate indentation (nested lists, code) is marked with this char so the
    // final pass can strip accidental leading whitespace without destroying it.
    const IND = '\u0001';

    function visible(el) {
        if (el.hasAttribute('data-s2pdf-hide') || el.hidden) return false;
        const cs = getComputedStyle(el);
        return cs.display !== 'none' && cs.visibility !== 'hidden';
    }
    const blockish = el => {
        const d = getComputedStyle(el).display;
        return !/^inline/.test(d) && d !== 'contents';
    };
    const esc = t => t.replace(/([\\*_\[\]`])/g, '\\$1');
    const oneLine = t => t.replace(/\s+/g, ' ').trim();
    const abs = u => { try { return new URL(u, location.href).href; } catch (e) { return ''; } };

    function children(node) {
        let out = '';
        for (const c of node.childNodes) out += conv(c);
        return out;
    }

    function list(el, ordered) {
        let i = 0, out = '\n\n';
        for (const li of el.children) {
            if (li.tagName !== 'LI' || !visible(li)) continue;
            i++;
            const marker = ordered ? i + '. ' : '- ';
            const body = children(li).trim().split('\n')
                .map((l, n) => n === 0 ? l : IND + IND + l).join('\n');
            out += marker + body + '\n';
        }
        return out + '\n';
    }

    function table(el) {
        const rows = [...el.querySelectorAll('tr')].filter(visible).map(tr =>
            [...tr.children].filter(c => /^(TD|TH)$/.test(c.tagName) && visible(c))
                .map(c => oneLine(children(c)).replace(/\|/g, '\\|')));
        if (!rows.length) return '';
        const n = Math.max(...rows.map(r => r.length));
        const line = r => '| ' + r.concat(Array(n - r.length).fill('')).join(' | ') + ' |';
        let out = '\n\n' + line(rows[0]) + '\n| ' + Array(n).fill('---').join(' | ') + ' |\n';
        for (const r of rows.slice(1)) out += line(r) + '\n';
        return out + '\n';
    }

    function conv(node) {
        if (node.nodeType === 3) return esc(node.nodeValue.replace(/\s+/g, ' '));
        if (node.nodeType !== 1) return '';
        const tag = node.tagName.toUpperCase();
        if (SKIP.has(tag) || !visible(node)) return '';
        switch (tag) {
            case 'H1': case 'H2': case 'H3': case 'H4': case 'H5': case 'H6': {
                const t = oneLine(children(node));
                return t ? '\n\n' + '#'.repeat(+tag[1]) + ' ' + t + '\n\n' : '';
            }
            case 'P': return '\n\n' + children(node).trim() + '\n\n';
            case 'BR': return '  \n';
            case 'HR': return '\n\n---\n\n';
            case 'STRONG': case 'B': { const t = children(node).trim(); return t ? '**' + t + '**' : ''; }
            case 'EM': case 'I': { const t = children(node).trim(); return t ? '*' + t + '*' : ''; }
            case 'CODE': return node.closest('pre') ? node.textContent : '`' + node.textContent + '`';
            case 'PRE':
                return '\n\n```\n' + node.textContent.replace(/\n+$/, '')
                    .replace(/^ +/gm, m => IND.repeat(m.length)) + '\n```\n\n';
            case 'BLOCKQUOTE':
                return '\n\n' + children(node).trim().split('\n').map(l => '> ' + l).join('\n') + '\n\n';
            case 'UL': return list(node, false);
            case 'OL': return list(node, true);
            case 'TABLE': return table(node);
            case 'DT': return '\n\n**' + oneLine(children(node)) + '**\n';
            case 'DD': return children(node).trim() + '\n';
            case 'SUMMARY': return '\n\n**' + oneLine(children(node)) + '**\n\n';
            case 'IMG': {
                if (!INCLUDE_IMAGES) return '';
                const src = node.currentSrc || node.src || '';
                if (!src || src.startsWith('data:')) return '';
                return '![' + oneLine(node.alt || '').replace(/[\[\]]/g, '') + '](' + abs(src) + ')';
            }
            case 'A': {
                const inner = oneLine(children(node));
                const href = node.getAttribute('href') || '';
                if (!inner) return '';
                if (!href || href.startsWith('#') || /^javascript:/i.test(href)) return inner;
                const u = abs(href);
                const link = /^(https?:|mailto:|tel:)/i.test(u) ? '[' + inner + '](' + u + ')' : inner;
                // Links laid out as blocks (flex/block "link lists") get their own paragraph
                return blockish(node) ? '\n\n' + link + '\n\n' : link;
            }
            case 'LI': case 'TR': case 'DL': case 'DETAILS':
                return '\n' + children(node) + '\n';
            default:
                return blockish(node) ? '\n' + children(node) + '\n' : children(node);
        }
    }

    let md = conv(root);
    md = md.replace(/[ \t]+\n/g, '\n')
           .replace(/^[ \t]+/gm, '')        // stray indentation would turn text into code blocks
           .replace(/\u0001/g, ' ')
           .replace(/\n{3,}/g, '\n\n')
           .trim();
    const title = oneLine(document.title || '');
    const prefix = (/^# /m.test(md) || !title) ? '' : '# ' + esc(title) + '\n\n';
    return JSON.stringify({ md: prefix + md });
})()
