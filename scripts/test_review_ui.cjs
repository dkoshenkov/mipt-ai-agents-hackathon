// Run against the local UI with Playwright available to Node.
const assert = require('node:assert/strict');
const { chromium } = require('playwright');

async function check(page, width) {
  await page.setViewportSize({ width, height: 800 });
  const source = { line: 2, quote: 'Шаг измерения указан без единицы.' };
  const procedure = { line: 1, quote: 'Электронный конкурс.' };
  const heading = '# Техническое задание\n';
  const range = {start: Buffer.byteLength(heading), end: Buffer.byteLength(heading + source.quote)};
  const standard = 'ГОСТ Р 53780-2010';
  let markdown = heading + source.quote;
  const references = ['supported', 'supported', 'outside_scope'].map(() => {
    markdown += '\n';
    const start = Buffer.byteLength(markdown);
    markdown += standard;
    return {existence: 'Exact', mention: {evidence: {range: {start, end: Buffer.byteLength(markdown)}}}};
  });
  markdown += '\n<table><tr><td><p>300</p></td></tr></table>\nГрузоподъёмность 1000 кг.';
  const span = text => {
    const index = markdown.indexOf(text);
    return {start: Buffer.byteLength(markdown.slice(0, index)), end: Buffer.byteLength(markdown.slice(0, index + text.length))};
  };
  const cellRange = span('<td><p>300</p></td>');
  const secondRange = span('Грузоподъёмность 1000 кг.');
  const report = {
    regime: 'uncertain', reason: 'В документах не указана категория заказчика.',
    applicability_source: null, coverage: [],
    findings: [{
      status: 'требует проверки', scope: 'Шаг измерения', category: 'Описание предмета закупки',
      source, explanation: 'Значение можно истолковать по-разному.',
      additional_sources: [], context: [], procedure_source: procedure, procedure_material: 'Порядок.pdf',
      basis: 'Пункт 2 технического задания.', missing: 'Уточнить единицу измерения.',
    }],
    consistency: {
      investigations: [{candidate: {object: 'Весы', property: 'Шаг'}, cited_evidence: [{range}],
        outcome: {Ok: {Confirmed: {explanation: 'Два разных шага измерения.'}}}}],
      object_questions: [], standard_material_questions: [], selections: [], standard_references: references,
      standard_usage: [{outcome: 'supported'}, {outcome: 'supported'}, {outcome: 'outside_scope'}],
      standard_coverage: {complete: true}, semantic_gaps: [], patch_verification: null,
    },
    equipment: {
      requirements: [{requirement: {position: 'Весы', property: 'Шаг',
        evidence: [{material_id: 'ТЗ.pdf', ...range}]},
        outcome: {status: 'pending', question: {text: 'Какую разницу масс нужно различать?'}}}],
      coverage: {review: {status: 'completed'}, gaps: [], blocks: [{extracted: true, reviewed: true}]},
    },
  };
  await page.route('**/api/convert', route => route.fulfill({
    json: { markdown: route.request().postData() === 'main' ? markdown : procedure.quote, warnings: [] },
  }));
  let finishReview;
  const reviewDelay = new Promise(resolve => { finishReview = resolve; });
  await page.route('**/api/review', async route => {
    const request = route.request().postDataJSON();
    assert.equal(request.public, true);
    assert.deepEqual(request.files.map(f => f.name), ['ТЗ.pdf', 'Порядок.pdf']);
    await reviewDelay;
    return route.fulfill({ json: report });
  });
  await page.goto(process.env.REVIEW_UI_URL || 'http://127.0.0.1:8711/');
  assert.deepEqual(await page.getByRole('heading').allTextContents(), ['Добавьте документы']);
  assert.equal(await page.locator('#report').isHidden(), true);
  await page.locator('#file-input').setInputFiles([
    { name: 'ТЗ.pdf', mimeType: 'application/pdf', buffer: Buffer.from('main') },
    { name: 'Порядок.pdf', mimeType: 'application/pdf', buffer: Buffer.from('appendix') },
  ]);
  assert.equal(await page.getByText('Основное ТЗ', {exact: true}).count(), 0);
  await page.getByRole('radio', {name: 'Порядок.pdf'}).check();
  await page.getByRole('radio', {name: 'ТЗ.pdf'}).check();
  await page.getByRole('button', { name: 'Подготовить документы' }).click();
  await page.locator('#public').waitFor();
  assert.equal(await page.locator('#review').isDisabled(), true);
  await page.getByRole('checkbox', {name: 'Все документы публичные. Разрешаю передать их текст внешним провайдерам.', exact: true}).check();
  await page.locator('#review').click();
  const checking = page.locator('.status.checking');
  assert.equal(await checking.innerText(), 'Проверяем документы…');
  assert.equal(await checking.evaluate(node => getComputedStyle(node, '::before').animationName), 'review-spin');
  await page.emulateMedia({reducedMotion: 'reduce'});
  assert.equal(await checking.evaluate(node => getComputedStyle(node, '::before').animationName), 'none');
  await page.emulateMedia({reducedMotion: 'no-preference'});
  finishReview();
  await page.getByText('Что нужно уточнить', { exact: true }).waitFor();
  assert.equal(await checking.count(), 0);
  const consistency = page.getByRole('region', { name: 'Согласованность ТЗ и ссылки на ГОСТ' });
  await consistency.getByText('Два разных шага измерения.').waitFor();
  const groups = consistency.locator('.standard-group');
  assert.equal(await groups.count(), 1);
  assert.equal(await consistency.getByText('Применимость подтверждена').count(), 0);
  assert.deepEqual(await groups.last().getByRole('link').allTextContents(), ['строка 5']);
  assert.equal(await groups.first().getByText(standard, {exact: true}).count(), 1);
  await consistency.getByRole('link', { name: 'строка 2' }).click();
  assert.equal(await page.locator(':target').innerText(), '2  ' + source.quote);
  await page.getByText('Какую разницу масс нужно различать?').waitFor();
  assert.equal(await page.locator('.status').innerText(), 'Проверка завершена.');
  await page.getByRole('link', { name: 'строка 1' }).click();
  assert.equal(await page.locator(':target').innerText(), '1  ' + procedure.quote);
  assert.equal(await page.locator('#preview-1').getAttribute('open'), '');
  await page.emulateMedia({ media: 'print' });
  assert.equal(await page.locator('.upload').isHidden(), true);
  assert.equal(await page.locator('#report').isVisible(), true);
  await page.emulateMedia({ media: 'screen' });
  assert.equal(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth), true);
  assert.equal(await page.locator('#report > section').first().getAttribute('aria-label'), 'Согласованность ТЗ и ссылки на ГОСТ');
  report.applicability_source = {line: 1, quote: '<a id="_Toc123"></a>'};
  const oldInvestigation = report.consistency.investigations[0];
  const comparison = {question:'Какая грузоподъёмность в кг верна?', sides:[{range:cellRange},{range:secondRange}]};
  report.consistency.investigations = [{candidate:{object:'Лифт', property:'Грузоподъёмность'},
    source_comparison:comparison, cited_evidence:[{range:cellRange},{range:cellRange}],
    outcome:{Ok:{Confirmed:{explanation:'Значения расходятся.', question:comparison.question}}}}];
  await page.locator('#review').click();
  await page.getByText(comparison.question, {exact:true}).waitFor();
  assert.equal(await page.getByText(comparison.question, {exact:true}).count(), 1);
  const conflict = consistency.locator('.finding').first();
  assert.deepEqual(await conflict.locator('blockquote').allTextContents(), ['300', 'Грузоподъёмность 1000 кг.']);
  assert.equal(await page.getByText('<a id="_Toc123"></a>', {exact:true}).count(), 0);
  report.consistency.investigations[0].outcome = {Err:'Model(TimedOut)'};
  await page.locator('#review').click();
  await page.locator('#report > .warning').waitFor();
  assert.equal(await consistency.locator('.finding .finding-meta').first().innerText(), 'Требует проверки');
  assert.deepEqual(await consistency.locator('.finding').first().locator('blockquote').allTextContents(), ['300', 'Грузоподъёмность 1000 кг.']);
  report.consistency.investigations[0].outcome = {Ok:{Dismissed:{}}};
  await page.locator('#review').click();
  await page.getByText('Нужно уточнить применимость 223-ФЗ').waitFor();
  assert.equal(await page.getByText(comparison.question, {exact:true}).count(), 0);
  report.consistency.investigations = [oldInvestigation];
  report.applicability_source = null;
  const equipment = report.equipment;
  report.equipment = {branch_error: 'Ошибка ветки оборудования.'};
  await page.locator('#review').click();
  await page.locator('#report').getByText('Ошибка ветки оборудования.').waitFor();
  assert.equal(await page.locator('.status').innerText(), 'Проверка выполнена частично.');
  assert.equal(await page.getByText('Что нужно уточнить', {exact: true}).isVisible(), true);
  assert.equal(await consistency.getByText('Два разных шага измерения.').isVisible(), true);
  report.equipment = equipment;
  report.findings = [];
  report.equipment = {...equipment, requirements: [
    ...equipment.requirements,
    ...['justified', 'insufficient', 'possibly_excessive'].map(assessment => ({
      requirement: {position: 'Весы', property:assessment, evidence:[]},
      outcome: {status:'assessed',assessment,reason:'Основание для ' + assessment,evidence:[]}
    }))
  ]};
  const failure = {stage:'assessment', reason:'TimedOut'};
  report.equipment.requirements.push({requirement:{id:'r1',position:'Лифт',property:'Вес',evidence:[]},outcome:{status:'stopped',failure}});
  report.equipment.coverage = {...report.equipment.coverage, gaps:[{failure,block_ids:['b1']}]};
  report.coverage = [
    { state: 'error', stage: 'plan', check: 'A01', detail: 'Нет ответа.' },
    ...Array.from({length: 5}, () => ({state: 'rejected', stage: 'plan', check: 'A06', detail: 'нет непустых основных строк'})),
  ];
  report.consistency.packets = [{stop:'Model(TimedOut)', binding_issues:[]},
    {stop:'BudgetExhausted', binding_issues:[]}, {stop:null, binding_issues:[{}]}];
  report.consistency.standard_coverage = {complete:false, unread_packets:[0,1,2]};
  await page.locator('#review').click();
  await page.locator('#report').getByText('Проверка выполнена частично', {exact: true}).waitFor();
  assert.equal(await page.getByText('Какую разницу масс нужно различать?').isVisible(), true);
  const summary = page.locator('#report > .warning');
  assert.equal(await summary.count(), 1);
  await summary.getByText(/строки для 5 правовых проверок/).waitFor();
  await summary.getByText(/ошибка запроса: 1; локальная остановка: 1; не все цитаты привязаны: 1/).waitFor();
  await summary.getByText(/требований: 1; фрагментов: 1/).waitFor();
  assert.equal(await page.locator('#report .warning details').count(), 0);
  assert.equal(await page.getByText('Основание для justified').count(), 0);
  assert.equal(await page.getByText('Недостаточно для задачи', {exact:true}).isVisible(), true);
  assert.equal(await page.getByText('Возможно избыточно', {exact:true}).isVisible(), true);
  report.consistency.investigations = [];
  report.consistency.standard_references = [];
  report.consistency.standard_usage = [];
  report.equipment = {branch_error:'Проверка оборудования остановлена.'};
  report.regime = 'applicable';
  await page.locator('#review').click();
  await page.getByText('Проверка оборудования остановлена.').waitFor();
  assert.equal(await page.getByText('Замечаний и ошибок не обнаружено.').count(), 0);
  const cleanEquipment = {requirements: [{requirement: {evidence: []}, outcome: {status: 'assessed', assessment: 'justified'}}],
    coverage: {review: {status: 'completed'}, gaps: [], blocks: []}};
  const clean = {regime:'applicable', reason:'Routine success', findings:[], coverage:[],
    consistency: {...report.consistency, investigations: [], standard_references: references.slice(0,2),
      standard_usage: [{outcome:'supported'}, {role:'bibliography',outcome:'insufficient_evidence'}],
      standard_coverage: {complete:false, uncertain_usage:[1]}}, equipment:cleanEquipment};
  await page.route('**/api/review', route => route.fulfill({json: clean}), {times:1});
  await page.locator('#review').click();
  const panel = page.locator('#report');
  await panel.getByText('Замечаний и ошибок не обнаружено.').waitFor();
  assert.equal(await panel.getByRole('region').count(), 0);
  assert.equal(await panel.getByText('Routine success').count(), 0);
  const downloaded = page.waitForEvent('download');
  await panel.getByRole('button', {name:'Скачать отчёт (.json)'}).click();
  assert.deepEqual(JSON.parse(require('node:fs').readFileSync(await (await downloaded).path(), 'utf8')), clean);
  await page.route('**/api/review', route => route.fulfill({ status: 503, json: { error: 'Сервис временно недоступен.' } }), {times: 1});
  await page.locator('#review').click();
  await page.getByText('Не удалось завершить проверку.', {exact: true}).waitFor();
  assert.equal(await checking.count(), 0);
  assert.equal(await page.locator('#review').isEnabled(), true);
  await page.getByRole('button', { name: 'Удалить ТЗ.pdf' }).click();
  assert.equal(await page.locator('#report').isHidden(), true);
  assert.equal(await page.locator('#review').isDisabled(), true);
}

(async () => {
  const browser = await chromium.launch({ executablePath: process.env.CHROMIUM_PATH });
  try {
    for (const width of [1280, 390]) {
      const page = await browser.newPage();
      await check(page, width);
      await page.close();
    }
    console.log('Три раздела отчёта, частичные результаты, ссылки и печать проверены при ширине 1280 и 390 пикселей.');
  } finally {
    await browser.close();
  }
})().catch(error => { console.error(error); process.exitCode = 1; });
