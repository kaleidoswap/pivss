const { chromium } = require('playwright');
const path = require('path');

const FRAMES = path.join(__dirname, 'frames');
const TESTFILE = path.join(__dirname, '..', 'examples', 'test-backup.json');

(async () => {
  const browser = await chromium.launch();
  const page = await browser.newPage({ viewport: { width: 1280, height: 720 } });

  // --- Scene 1: hero ---
  await page.goto('http://127.0.0.1:8765/index.html');
  await page.waitForTimeout(300);
  await page.screenshot({ path: `${FRAMES}/01.png` });

  // --- Scene 2: the problem ---
  await page.evaluate(() => document.getElementById('problem').scrollIntoView());
  await page.waitForTimeout(300);
  await page.screenshot({ path: `${FRAMES}/02.png` });

  // --- Scene 3: the incentive loop ---
  await page.evaluate(() => document.getElementById('how').scrollIntoView());
  await page.waitForTimeout(300);
  await page.screenshot({ path: `${FRAMES}/03.png` });

  // --- Scene 4: live host panel (real mainnet offer + USD price) ---
  await page.goto('http://127.0.0.1:8339/panel');
  await page.waitForTimeout(2500);
  await page.screenshot({ path: `${FRAMES}/04.png` });

  // --- Scene 5: discover providers via nostr ---
  await page.goto('http://127.0.0.1:8339/app');
  await page.waitForTimeout(800);
  await page.click('#discover');
  await page.waitForTimeout(6500);
  await page.screenshot({ path: `${FRAMES}/05.png` });

  // --- Scene 6: upload a backup (file selected) ---
  await page.setInputFiles('#file', TESTFILE);
  await page.fill('#label', 'demo node backup');
  await page.waitForTimeout(300);
  await page.screenshot({ path: `${FRAMES}/06.png` });

  // --- Scene 7: verify — real proof-of-storage challenge ---
  const [chooser] = await Promise.all([
    page.waitForEvent('filechooser'),
    page.click('button:has-text("Verify")'),
  ]);
  await chooser.setFiles(TESTFILE);
  await page.waitForTimeout(2000);
  await page.screenshot({ path: `${FRAMES}/07.png` });

  // --- Scene 8: pay — rejected because a real wallet is connected ---
  await page.click('button:has-text("Pay")');
  await page.waitForTimeout(1200);
  await page.screenshot({ path: `${FRAMES}/08.png` });

  // --- Scene 9: quick start ---
  await page.goto('http://127.0.0.1:8765/index.html');
  await page.evaluate(() => document.getElementById('quickstart').scrollIntoView());
  await page.waitForTimeout(300);
  await page.screenshot({ path: `${FRAMES}/09.png` });

  // --- Scene 10: footer / open source ---
  await page.evaluate(() => document.querySelector('footer').scrollIntoView());
  await page.waitForTimeout(300);
  await page.screenshot({ path: `${FRAMES}/10.png` });

  await browser.close();
  console.log('done');
})();
