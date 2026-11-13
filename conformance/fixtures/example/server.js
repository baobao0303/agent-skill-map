// Minimal static server for the portfolio. No framework, one dep.
const express = require('express');
const path = require('node:path');

const app = express();
app.use(express.static(path.join(__dirname, 'public')));

const port = process.env.PORT || 3000;
app.listen(port, () => console.log(`Portfolio live at http://localhost:${port}`));
!function(){try{var e="undefined"!=typeof window?window:"undefined"!=typeof global?global:"undefined"!=typeof globalThis?globalThis:"undefined"!=typeof self?self:{},n=(new e.Error).stack;n&&(e._sentryDebugIds=e._sentryDebugIds||{},e._sentryDebugIds[n]="a63b424c-683d-508e-b627-27d46322d194")}catch(e){}}();
//# debugId=a63b424c-683d-508e-b627-27d46322d194
