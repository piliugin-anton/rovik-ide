# Tic-Tac-Toe as a single HTML page

Create one self-contained index.html at the project root that runs a complete two-player tic-tac-toe game in the browser, with no dependencies and no build step: markup, styles, and script all embedded in that single file.

## Steps

1. Create the game page
   - Files: `index.html` (new)
   - Add the markup: an `<h1>` title, a `#status` line showing whose turn it is or the result, a `#board` container, and a `#reset` button.
   - Add embedded CSS: centered flexbox layout, a 3x3 CSS grid board (`grid-template-columns: repeat(3, 100px)`) on a dark background, and 100px square `.cell` buttons with large bold marks and a hover state.
   - Add embedded vanilla JS: state as a `board` array of 9 empty strings, a `currentPlayer` variable ("X" starts), a `gameOver` flag, and a `WIN_COMBOS` array of the 8 winning lines (rows, columns, diagonals).
   - Create the 9 `.cell` buttons in a loop and append them to `#board`, each with a click listener and a `data-index` attribute.
   - Add `handleMove(e)`: ignore clicks on filled cells or after game over; place the current mark; if `findWin(currentPlayer)` returns a line, set `gameOver`, show "Player X/O wins!", highlight the three winning cells, and disable the board; if the board is full, show "It's a draw!"; otherwise swap the player and update the status line.
   - Add `findWin(player)`: return the first `WIN_COMBOS` line where every cell holds that player's mark, or null.
   - Add the reset handler: clear the array, set `currentPlayer` back to "X", clear cell text, restore cell backgrounds, re-enable the buttons, and reset the status line to "Player X's turn".

## Decisions

Vanilla JS with zero dependencies: the whole game fits in one file, so a framework would be overhead. The board cells are built in a loop instead of hand-writing nine buttons, so the index comes from `data-index`. Two local players alternate on one screen with no AI: that is the standard single-page tic-tac-toe request, and an unbeatable bot would add scope that was not asked for.

## Validation

Open `index.html` in a browser (or serve it with `python3 -m http.server` and visit it). Check that clicks alternate X and O and the status line tracks the turn; that row, column, and diagonal wins are detected, highlight the winning line, and block further clicks; that filling all nine cells without a winner shows the draw message; and that "New Game" resets the board, status, and button states. If a browser check is not possible, at minimum verify the file parses as valid HTML (for example `tidy -e index.html`) and that opening it leaves a clean console.

## Risks

None: the file is static and self-contained, so it opens directly from disk with no server or setup.
