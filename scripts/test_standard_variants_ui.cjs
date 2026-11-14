// Run with Playwright available to Node; no network or provider calls.
const assert = require('node:assert/strict');
const path = require('node:path');
const { pathToFileURL } = require('node:url');
const { chromium } = require('playwright');
const root = path.join(__dirname, '..', 'ui');
(async () => {
  const browser = await chromium.launch({executablePath: process.env.CHROMIUM_PATH});
  try {
    const page = await browser.newPage();
    await page.goto(pathToFileURL(path.join(root, 'index.html')).href);
    await page.evaluate(() => {
      const text = 'ГОСТ Р 34441-2018';
      primary = {file: {name: 'ТЗ.pdf'}, result: {markdown: text}};
      files.push(primary);
      document.querySelector('#report').hidden = false;
      const ref = {existence: 'NotFoundInOfficialSearch', mention: {evidence: {range: {start: 0, end: new TextEncoder().encode(text).length}}},
        confirmed_family_variants: [{designation:'ГОСТ 34441-2018', title:'Лифты. Диспетчерский контроль. Общие технические требования', status_at_retrieval:'Заменен', replacement:'ГОСТ 34441-2024', url:'https://protect.gost.ru/gost/details/f8bc51af-213e-44f5-95e3-13ff33a69a69'}]};
      showStandards(document.querySelector('#report'), {standard_references:[ref], standard_usage:[]});
    });
    const group = page.locator('.standard-group');
    assert.equal(await group.locator('h5').innerText(), 'Расхождение в обозначении ГОСТ');
    const text = await group.innerText();
    for (const value of ['В ТЗ: ГОСТ Р 34441-2018', 'В каталоге: ГОСТ 34441-2018', 'Заменен', 'ГОСТ 34441-2024', 'точном поиске']) assert.ok(text.includes(value), value);
    assert.equal(await group.getByRole('link', {name:'Официальная карточка'}).getAttribute('href'), 'https://protect.gost.ru/gost/details/f8bc51af-213e-44f5-95e3-13ff33a69a69');
    await page.evaluate(() => {
      const section = document.querySelector('#report'); section.replaceChildren();
      const ref = {existence:'NotFoundInOfficialSearch', mention:{evidence:{range:{start:0, end:new TextEncoder().encode('ГОСТ Р 34441-2018').length}}},
        confirmed_family_variants:[{designation:'ГОСТ 34441-2018', title:'Лифты', status_at_retrieval:'Заменен', url:'https://protect.gost.ru.evil.example/gost/details/f8bc51af-213e-44f5-95e3-13ff33a69a69'}]};
      showStandards(section, {standard_references:[ref], standard_usage:[]});
    });
    assert.equal(await group.getByRole('link', {name:'Официальная карточка'}).count(), 0);
    await page.evaluate(() => {
      const section = document.querySelector('#report'); section.replaceChildren();
      const ref = {existence:'NotFoundInOfficialSearch', mention:{evidence:{range:{start:0, end:new TextEncoder().encode('ГОСТ Р 34441-2018').length}}}};
      showStandards(section, {standard_references:[ref], standard_usage:[]});
    });
    assert.equal(await group.locator('h5').innerText(), 'Не найден в официальном каталоге');
    console.log('Отображение вариантов ГОСТ и старого отчёта проверено.');
  } finally { await browser.close(); }
})().catch(error => { console.error(error); process.exitCode = 1; });
