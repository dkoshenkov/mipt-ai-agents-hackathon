const $ = selector => document.querySelector(selector);
const files = [];
let primary;
let busy = false;

function el(tag, text, className) {
  const item = document.createElement(tag);
  if (text !== undefined) item.textContent = text;
  if (className) item.className = className;
  return item;
}

function resetReport() {
  $('#report').replaceChildren();
  $('#report').hidden = true;
  $('#public').checked = false;
  $('.status').textContent = '';
}

function preview(entry, index) {
  const details = el('details');
  details.id = 'preview-' + index;
  details.append(el('summary', 'Посмотреть текст документа'));
  const pre = el('pre');
  entry.result.markdown.split('\n').forEach((line, i) => {
    const row = el('span', (i + 1) + '  ' + line);
    row.id = details.id + '-L' + (i + 1);
    pre.append(row);
  });
  const save = el('button', 'Скачать текст (.md)', 'text-button');
  save.onclick = () => download(entry.file.name.replace(/\.(docx|pdf)$/i, '.md'), entry.result.markdown, 'text/markdown');
  details.append(pre, save);
  return details;
}

function render() {
  const pending = files.some(entry => !entry.result);
  $('#convert').hidden = !pending;
  $('#convert').disabled = busy;
  $('#file-input').disabled = busy;
  const list = $('#file-list');
  const open = new Set([...list.querySelectorAll('details[open]')].map(node => node.closest('.file-row').dataset.name));
  list.replaceChildren();
  if (files.length > 1) list.append(el('p', 'Выберите файл с техническим заданием.', 'file-prompt'));
  for (const [index, entry] of files.entries()) {
    const row = el('div', undefined, 'file-row');
    row.dataset.name = entry.file.name;
    const heading = el('div', undefined, 'file-heading');
    const name = el('b', entry.file.name);
    if (files.length > 1) {
      const label = el('label');
      const radio = el('input');
      radio.type = 'radio';
      radio.name = 'primary';
      radio.checked = entry === primary;
      radio.disabled = busy;
      radio.onchange = () => { primary = entry; resetReport(); render(); };
      label.append(radio, name);
      heading.append(label);
    } else {
      heading.append(name);
    }
    const remove = el('button', 'Удалить', 'remove');
    remove.disabled = busy;
    remove.setAttribute('aria-label', 'Удалить ' + entry.file.name);
    remove.onclick = () => {
      files.splice(index, 1);
      if (primary === entry) primary = files[0];
      resetReport(); render();
    };
    heading.append(remove);
    row.append(heading);
    if (entry.error) row.append(el('p', entry.error, 'error'));
    if (entry.result) {
      for (const warning of entry.result.warnings) row.append(el('p', warning, 'warning'));
      const details = preview(entry, index);
      details.open = open.has(entry.file.name);
      row.append(details);
    }
    list.append(row);
  }
  $('#review-actions').hidden = files.length === 0 || pending;
  $('#public').disabled = busy;
  $('#review').disabled = busy || pending || !$('#public').checked;
}
function download(name, text, mime) {
  const url = URL.createObjectURL(new Blob([text], {type: mime}));
  const link = el('a');
  link.href = url;
  link.download = name;
  link.click();
  setTimeout(() => URL.revokeObjectURL(url), 1000);
}

function add(incoming) {
  if (busy) return;
  const errors = [];
  for (const file of incoming) {
    if (!/\.(docx|pdf)$/i.test(file.name) || file.size > 20 * 1024 * 1024) {
      errors.push(file.name + ' — нужен DOCX или PDF до 20 МиБ.');
    } else if (files.some(f => f.file.name === file.name)) {
      errors.push(file.name + ' — файл с таким именем уже добавлен.');
    } else {
      files.push({file});
      primary ??= files[0];
      resetReport();
    }
  }
  $('#error').textContent = errors.join(' ');
  $('#file-input').value = '';
  render();
}

async function post(path, type, body) {
  const response = await fetch(path, {method: 'POST', headers: {'Content-Type': type}, body});
  if (response.status === 413) throw new Error('Размер запроса превышает 20 МиБ.');
  const data = await response.json();
  if (!response.ok) throw new Error(data.error || 'Ошибка сервера.');
  return data;
}

$('#convert').onclick = async () => {
  busy = true;
  $('#error').textContent = '';
  render();
  try {
    for (const entry of files.filter(f => !f.result)) {
      $('.status').textContent = 'Читаем документ «' + entry.file.name + '»…';
      entry.error = '';
      try {
        entry.result = await post('/api/convert', 'application/octet-stream', entry.file);
      } catch (error) {
        entry.error = error.message;
      }
      render();
    }
  } finally {
    busy = false;
    $('.status').textContent = files.some(f => f.error) ? 'Не удалось подготовить все файлы. Проверьте сообщение рядом с документом и повторите попытку.'
      : 'Текст готов. Перед проверкой его можно сверить с документами.';
    render();
  }
};

function lineLink(line, documentName = primary.file.name) {
  const index = files.findIndex(entry => entry.file.name === documentName);
  const target = 'preview-' + index + '-L' + line;
  const anchor = el('a', 'строка ' + line);
  if (document.getElementById(target)) {
    anchor.href = '#' + target;
    anchor.onclick = () => { document.getElementById('preview-' + index).open = true; };
  }
  return anchor;
}

function citation(label, location, documentName = primary.file.name) {
  const block = el('div', undefined, 'citation');
  const caption = el('div', label + ' · ' + documentName + ' · ', 'source-label');
  caption.append(lineLink(location.line, documentName));
  block.append(caption, el('blockquote', location.quote));
  return block;
}

function findingItem(finding) {
  const item = el('article', undefined, 'finding');
  const meta = el('p', [finding.status, finding.scope && finding.category].filter(Boolean).join(' · '), 'finding-meta');
  item.append(meta, el('h4', finding.scope || finding.category), el('p', finding.explanation));
  if (finding.source) item.append(citation('В техническом задании', finding.source));
  for (const loc of finding.additional_sources || []) item.append(citation('Также в ТЗ', loc));
  for (const loc of finding.context || []) item.append(citation('Условие', loc));
  if (finding.procedure_source) {
    item.append(citation('Способ закупки', finding.procedure_source, finding.procedure_material || primary.file.name));
  }
  for (const [title, text] of [
    ['Что нужно уточнить', finding.missing],
    ['Предлагаемая формулировка', finding.wording],
    ['Почему предложена правка', finding.wording_reason],
    ['Основание замечания', finding.basis],
  ]) {
    if (text) item.append(el('h5', title), el('p', text));
  }
  return item;
}

function showLegal(summary, report, incomplete) {
  const gaps = (report.coverage || []).filter(c => ['error', 'restricted', 'rejected'].includes(c.state)
    || (c.stage === 'plan' && c.state === 'skipped'));
  if (gaps.length) {
    const noLines = gaps.filter(g => g.detail?.includes('нет непустых основных строк')).length;
    if (noLines) incomplete.push('Не удалось определить строки для ' + noLines + ' правовых проверок.');
    const other = gaps.length - noLines;
    if (other) incomplete.push('Не завершено правовых проверок: ' + other + '.');
  }
  const findings = [...(report.findings || [])].sort((a, b) => (a.source?.line || 0) - (b.source?.line || 0));
  summary.append(...findings.map(findingItem));
  if (report.regime === 'uncertain') {
    summary.append(el('h4', 'Нужно уточнить применимость 223-ФЗ'));
    if (report.reason) summary.append(el('p', report.reason));
    if (report.applicability_source && !/^<a\s+id=["']_Toc[^"']*["']\s*><\/a>$/i.test(report.applicability_source.quote?.trim() || '')) {
      summary.append(citation('Основание', report.applicability_source));
    }
  }
  return gaps.length > 0;
}

// Both additional branches address the converted Markdown in UTF-8 bytes.
function sourceLocation(entry, range) {
  if (!entry.bytes) {
    entry.bytes = new TextEncoder().encode(entry.result.markdown);
    entry.lines = [0];
    for (let i = entry.bytes.indexOf(10); i >= 0; i = entry.bytes.indexOf(10, i + 1)) {
      entry.lines.push(i + 1);
    }
  }
  const line = 1 + entry.lines.findLastIndex(offset => offset <= range.start);
  const quote = new TextDecoder().decode(entry.bytes.subarray(range.start, range.end));
  return { line, quote };
}

function byteCitation(label, range, name = primary.file.name) {
  const entry = files.find(f => f.file.name === name);
  return entry ? citation(label, sourceLocation(entry, range), name) : el('blockquote', range.quote);
}

function officialCardLink(address) {
  try {
    const url = new URL(address);
    if (url.origin !== 'https://protect.gost.ru' || url.username || url.password
      || !/^\/gost\/details\/[0-9a-f-]{36}$/i.test(url.pathname)) return null;
    const link = el('a', 'Официальная карточка');
    link.href = url.href;
    link.target = '_blank';
    link.rel = 'noopener noreferrer';
    return link;
  } catch (_) { return null; }
}

function showStandards(section, report) {
  if (!report.standard_references.length) return;
  const labels = {
    outside_scope: 'Требование вне области применения',
    insufficient_evidence: 'Недостаточно сведений о применимости',
  };
  const groups = new Map();
  for (const [i, ref] of report.standard_references.entries()) {
    const usage = report.standard_usage?.[i] || {};
    const missing = ref.existence === 'NotFoundInOfficialSearch';
    const variants = missing ? ref.confirmed_family_variants || [] : [];
    const status = ref.date_status?.find(s => s === 'BeforeEffective' || /отмен[её]н|замен[её]н|утратил силу|не действует/i.test(s.SnapshotStatus || ''));
    const catalog = variants.length ? 'Расхождение в обозначении ГОСТ'
      : missing ? 'Не найден в официальном каталоге'
      : status === 'BeforeEffective' ? 'Ещё не вступил в действие на дату проверки' : status?.SnapshotStatus;
    const bibliographyOnly = usage.role === 'bibliography' && usage.outcome === 'insufficient_evidence';
    const usageIssue = bibliographyOnly ? undefined : labels[usage.outcome];
    if (!catalog && !usageIssue && !usage.editorial_correction) continue;
    const title = catalog || usageIssue || 'Требуется исправить ссылку на стандарт';
    const note = [variants.length ? 'Указанное в ТЗ обозначение не найдено при точном поиске.' : '', catalog && usage.outcome === 'outside_scope' ? usageIssue : '', bibliographyOnly || usage.outcome === 'technical_failure' ? '' : usage.limitation].filter(Boolean).join(' ');
    const key = JSON.stringify([title, note]);
    if (!groups.has(key)) groups.set(key, {title, note, standards: new Map()});
    const {standards} = groups.get(key);
    const {quote, line} = sourceLocation(primary, ref.mention.evidence.range);
    const name = quote.trim().replace(/\s+/g, ' ');
    if (!standards.has(name)) standards.set(name, {lines: new Set(), variants: new Map(), citations: new Map()});
    const item = standards.get(name);
    item.lines.add(line);
    for (const card of variants) if (card?.url) item.variants.set(card.url, card);
    if (usage.official_url) item.citations.set(usage.official_url, usage.official_excerpt || '');
    if (!missing && status) for (const card of ref.cards || []) {
      if (card.url && !item.citations.has(card.url)) item.citations.set(card.url, '');
    }
  }
  if (!groups.size) return;
  section.append(el('h4', 'Замечания к ссылкам на ГОСТ', 'subheading'));
  for (const {title, note, standards} of groups.values()) {
    const group = el('div', undefined, 'standard-group');
    group.append(el('h5', title));
    if (note) group.append(el('p', note));
    const list = el('ul');
    for (const [name, {lines, variants, citations}] of standards) {
      const row = el('li');
      const links = el('span', undefined, 'line-links');
      for (const line of lines) links.append(lineLink(line));
      row.append(el('b', variants.size ? 'В ТЗ: ' + name : name), links);
      for (const card of variants.values()) {
        const details = el('p');
        details.append(document.createTextNode('В каталоге: ' + card.designation + ' — ' + card.title + '. Статус карточки: ' + card.status_at_retrieval + (card.replacement ? '. Заменяющий: ' + card.replacement : '') + '. '));
        const link = officialCardLink(card.url);
        if (link) details.append(link);
        row.append(details);
      }
      for (const [url, excerpt] of citations) {
        const link = officialCardLink(url);
        if (!link || variants.has(url)) continue;
        row.append(link);
        if (excerpt) row.append(el('blockquote', excerpt));
      }
      list.append(row);
    }
    group.append(list);
    section.append(group);
  }
}

function showConsistency(section, report, incomplete) {
  const conclusions = [];
  const selections = new Map(report.selections || []);
  const displayedQuestions = new Set();
  for (const investigation of report.investigations || []) {
    const verdict = investigation.outcome?.Ok;
    if (verdict?.Dismissed) continue;
    const comparison = investigation.source_comparison;
    const decision = verdict?.Confirmed || verdict?.NeedsReview;
    if (!decision && !(investigation.outcome?.Err !== undefined && comparison)) continue;
    const { object, property } = investigation.candidate || {};
    const id = investigation.candidate?.id;
    const question = selections.get(id)?.Selected ? null : comparison?.question;
    if (question) displayedQuestions.add(id);
    const item = el('article', undefined, 'finding');
    item.append(el('p', verdict?.Confirmed ? 'Подтверждено' : 'Требует проверки', 'finding-meta'),
      el('h4', question || [object, property].filter(Boolean).join(' · ')));
    if (decision?.explanation) item.append(el('p', decision.explanation));
    if (decision?.question && !question) item.append(el('p', decision.question));
    const evidence = comparison?.sides || [...(investigation.cited_evidence || [])]
      .sort((a, b) => (b.range?.end - b.range?.start || 0) - (a.range?.end - a.range?.start || 0));
    const seen = new Set();
    const ranges = [];
    for (const ref of evidence) {
      const range = ref.range;
      if (!range) continue;
      const key = range.start + ':' + range.end;
      if (seen.has(key) || (!comparison && ranges.some(r => range.start < r.end && r.start < range.end))) continue;
      seen.add(key);
      ranges.push(range);
      const location = sourceLocation(primary, range);
      const tableCell = /^\s*<td(?:\s[^>]*)?>[\s\S]*<\/td>\s*$/i.test(location.quote);
      if (tableCell) {
        const template = document.createElement('template');
        template.innerHTML = location.quote;
        location.quote = template.content.textContent.trim();
      }
      item.append(citation(tableCell ? 'Значение в таблице' : 'В ТЗ', location));
      if (seen.size === 2) break;
    }
    conclusions.push(item);
  }
  section.append(...conclusions);
  for (const question of [...(report.object_questions || []), ...(report.standard_material_questions || [])]) {
    section.append(el('p', question.question), byteCitation('В ТЗ', question.reference.evidence.range));
  }
  for (const [id, selection] of selections) {
    if (selection.AskUser && !displayedQuestions.has(id)) {
      section.append(el('p', selection.AskUser.question));
    }
  }
  if (report.patch_verification === 'Verified') {
    for (const patch of report.patches) {
      section.append(byteCitation('Предлагаемая правка', patch.range), el('p', 'Заменить на «' + patch.replacement + '».'));
    }
  }
  const warnings = [];
  const coverage = report.standard_coverage || {};
  if (coverage.unread_packets?.length) {
    const packets = report.packets || [];
    const affected = coverage.unread_packets.map(i => packets[i]).filter(Boolean);
    const request = affected.filter(p => /^Model\(/.test(p.stop || '')).length;
    const local = affected.filter(p => p.stop && !/^Model\(/.test(p.stop)).length;
    const binding = affected.filter(p => p.binding_issues?.length).length;
    const reasons = [request && 'ошибка запроса: ' + request, local && 'локальная остановка: ' + local,
      binding && 'не все цитаты привязаны: ' + binding].filter(Boolean);
    warnings.push('Не полностью проверены фрагменты ТЗ: ' + coverage.unread_packets.length
      + (packets.length ? ' из ' + packets.length : '') + (reasons.length ? ' (' + reasons.join('; ') + ')' : '') + '.');
  }
  if (coverage.parsing_gaps?.length || coverage.oversized_blocks?.length) warnings.push('Часть исходного текста не удалось прочитать.');
  if (coverage.failed_catalog?.length) warnings.push('Ошибка запроса к официальному каталогу (' + coverage.failed_catalog.length + ').');
  if (coverage.partial_references?.length) warnings.push('Поиск обозначений ГОСТ завершён частично (' + coverage.partial_references.length + ').');
  if (coverage.failed_usage?.length) warnings.push('Не удалось проверить применимость ГОСТ (' + coverage.failed_usage.length + ').');
  if ((report.semantic_gaps || []).length || (report.investigations || []).some(i => i.outcome?.Err !== undefined)
      || (report.selections || []).some(([, selection]) => selection.Unverified)) {
    warnings.push('Часть требований или расхождений не удалось проверить.');
  }
  if (report.patch_verification && report.patch_verification !== 'Verified') warnings.push('Предложенные правки не прошли проверку на копии.');
  incomplete.push(...warnings);
  showStandards(section, report);
  const onlyBibliography = coverage.uncertain_usage?.length > 0
    && coverage.uncertain_usage.every(i => report.standard_usage?.[i]?.role === 'bibliography')
    && Object.entries(coverage).every(([key, value]) => key === 'uncertain_usage' || !Array.isArray(value) || !value.length);
  const incompleteCoverage = !coverage.complete && !onlyBibliography;
  if (incompleteCoverage && !warnings.length) incomplete.push('Текст и ссылки на стандарты проверены частично.');
  return incompleteCoverage || warnings.length > 0;
}

function showEquipment(section, report, incomplete) {
  const coverage = report.coverage;
  const errors = new Map();
  const stages = {index:'Чтение документа', decode:'Преобразование', extract:'Извлечение требований',
    obligation_scope:'Область действия требования', assessment:'Оценка требований', final_review:'Финальная сверка',
    review_note:'Замечание ревьюера', reconcile:'Сверка с исходным текстом', read:'Чтение фрагмента', source:'Проверка источника'};
  const reasons = {invalid_response:'некорректный ответ модели', InvalidResponse:'некорректный ответ модели',
    unreadable:'не удалось однозначно прочитать текст', context_limit:'превышен лимит контекста',
    budget_exhausted:'исчерпан бюджет вызовов', http_budget_exhausted:'исчерпан бюджет запросов к источникам',
    TimedOut:'истекло время ожидания', timed_out:'истекло время ожидания', RateLimited:'достигнут лимит провайдера',
    Provider:'ошибка провайдера', restricted_context:'ограничен доступ к материалам'};
  function group(failure) {
    const raw = failure?.reason;
    const reason = typeof raw === 'object' && raw ? (raw.model || Object.keys(raw)[0]) : raw;
    const title = (stages[failure?.stage] || failure?.stage || 'Проверка требований')
      + ' — ' + (reasons[reason] || reason || 'ошибка проверки');
    if (!errors.has(title)) errors.set(title, {items: new Set(), blocks: new Set(), other: 0});
    return errors.get(title);
  }
  for (const {requirement, outcome} of report.requirements) {
    if (outcome.status === 'assessed' && outcome.assessment === 'justified') continue;
    const name = [requirement.position, requirement.component, requirement.property].filter(Boolean).join(' · ');
    if (outcome.status === 'stopped') {
      group(outcome.failure).items.add(requirement.id || name);
      continue;
    }
    const item = el('article', undefined, 'finding');
    item.append(el('h4', name));
    if (outcome.status === 'assessed') {
      item.append(el('p', outcome.assessment === 'insufficient' ? 'Недостаточно для задачи' : 'Возможно избыточно', 'finding-meta'), el('p', outcome.reason));
    } else {
      item.append(el('p', outcome.question?.text || outcome.missing_fact));
    }
    const sources = el('details');
    sources.append(el('summary', 'Цитаты из документов'));
    if (requirement.reference) sources.append(byteCitation('Ссылка на приложение в ТЗ', requirement.reference, requirement.reference.material_id));
    for (const ref of requirement.evidence) sources.append(byteCitation('Требование', ref, ref.material_id));
    for (const ref of outcome.evidence || []) sources.append(byteCitation('Основание оценки', ref, ref.material_id));
    if (sources.childElementCount > 1) item.append(sources);
    section.append(item);
  }
  for (const gap of coverage.gaps) {
    const entry = group(gap.failure);
    if (gap.block_ids?.length) for (const id of gap.block_ids) entry.blocks.add(id);
    else entry.other++;
  }
  if (coverage.review.status === 'failed') group(coverage.review.failure);
  for (const [title, {items, blocks, other}] of errors) {
    const counts = [items.size && 'требований: ' + items.size, blocks.size && 'фрагментов: ' + blocks.size,
      other && 'остановок: ' + other].filter(Boolean);
    incomplete.push(title + (counts.length ? ' (' + counts.join('; ') + ')' : '') + '.');
  }
  const partial = errors.size > 0 || coverage.review.status !== 'completed'
    || coverage.blocks.some(block => !block.extracted || !block.reviewed);
  if (partial && !errors.size) incomplete.push('Часть требований к оборудованию не проверена.');
  return partial;
}

function showReport(report) {
  const panel = $('#report');
  const heading = el('div', undefined, 'report-heading');
  const title = el('h2', 'Замечания и ошибки');
  title.id = 'report-title';
  title.tabIndex = -1;
  const documentInfo = el('div');
  documentInfo.append(title, el('p', [primary, ...files.filter(f => f !== primary)].map(f => f.file.name).join(' · ')));
  const print = el('button', 'Распечатать отчёт');
  print.onclick = () => window.print();
  const save = el('button', 'Скачать отчёт (.json)');
  save.onclick = () => download('review.json', JSON.stringify(report, null, 2), 'application/json');
  heading.append(documentInfo, print, save);
  panel.replaceChildren(heading);
  let partial = false;
  const incomplete = [];
  for (const [label, branch, renderBranch] of [
    ['Согласованность ТЗ и ссылки на ГОСТ', report.consistency, showConsistency],
    ['Проверка по 223-ФЗ', report, showLegal],
    ['Обоснованность требований к оборудованию', report.equipment, showEquipment],
  ]) {
    const section = el('section', undefined, 'report-summary');
    section.setAttribute('aria-label', label);
    if (!branch || branch.branch_error) {
      incomplete.push(label + ': ' + (branch?.branch_error || 'сервер не вернул результат проверки.'));
      partial = true;
    } else if (renderBranch(section, branch, incomplete)) {
      partial = true;
    }
    if (section.childElementCount) {
      section.prepend(el('h3', label));
      panel.append(section);
    }
  }
  if (panel.childElementCount === 1 && !partial) panel.append(el('p', 'Замечаний и ошибок не обнаружено.'));
  if (incomplete.length) {
    const warning = el('div', undefined, 'warning');
    warning.append(el('strong', 'Проверка выполнена частично'), el('p', incomplete.join(' ')));
    panel.append(warning);
  }
  panel.hidden = false;
  $('.status').textContent = partial ? 'Проверка выполнена частично.' : 'Проверка завершена.';
  title.focus();
}

$('#review').onclick = async () => {
  if (busy || !$('#public').checked) return;
  busy = true;
  render();
  $('#error').textContent = '';
  $('#report').hidden = true;
  const status = $('.status');
  status.textContent = 'Проверяем документы…';
  status.classList.add('checking');
  try {
    const ordered = [primary, ...files.filter(f => f !== primary)];
    const report = await post('/api/review', 'application/json', JSON.stringify({
      public: true, files: ordered.map(f => ({name: f.file.name, markdown: f.result.markdown})),
    }));
    showReport(report);
  } catch (error) {
    status.textContent = 'Не удалось завершить проверку.';
    $('#error').textContent = error.message;
  } finally {
    status.classList.remove('checking');
    busy = false; render();
  }
};
$('#public').onchange = () => { $('#review').disabled = !$('#public').checked; };
// Print the evidence for visible findings, then restore the reader’s selection.
window.addEventListener('beforeprint', () => {
  for (const details of document.querySelectorAll('#report .finding details:not([open])')) {
    details.dataset.printOpened = '';
    details.open = true;
  }
});
window.addEventListener('afterprint', () => {
  for (const details of document.querySelectorAll('#report details[data-print-opened]')) {
    details.open = false;
    delete details.dataset.printOpened;
  }
});
$('#file-input').onchange = event => add(event.target.files);
const zone = $('#dropzone');
for (const name of ['dragenter', 'dragover', 'dragleave', 'drop']) {
  zone.addEventListener(name, event => {
    event.preventDefault();
    zone.classList.toggle('drag', !busy && ['dragenter', 'dragover'].includes(name));
    if (name === 'drop') add(event.dataTransfer.files);
  });
}
render();
