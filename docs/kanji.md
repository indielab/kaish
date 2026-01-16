# 漢字 (Kanji) Reference for kaish

Vocabulary for the kaish project, organized by concept. Use this as a reference when naming features, writing comments, or adding flair to the codebase.

## Currently Used

| 漢字 | Reading | Meaning | Usage in kaish |
|------|---------|---------|----------------|
| 会 | kai / ai | meeting, gathering | Project name: 会sh (kaish) |
| 術 | jutsu | technique, art | Parent project: 会術 (Kaijutsu) |
| 核 | kaku | kernel, nucleus, core | The kernel: 核 |
| 散 | san / chiru | scatter, disperse | `scatter` builtin |
| 集 | shū / atsumeru | gather, collect | `gather` builtin |

---

## Execution & Control Flow

| 漢字 | Reading | Meaning | Potential Use |
|------|---------|---------|---------------|
| 実行 | jikkō | execution | Command execution |
| 流 | ryū / nagare | flow, stream | Pipes, data flow |
| 並 | hei / narabu | parallel, line up | Parallelism |
| 並列 | heiretsu | parallel (adj) | Parallel execution |
| 待 | tai / matsu | wait | `wait` builtin |
| 走 | sō / hashiru | run | Running processes |
| 止 | shi / tomeru | stop | Stopping/cancelling |
| 繰 | kuri / kurikaeru | repeat | Loops |

---

## Data & State

| 漢字 | Reading | Meaning | Potential Use |
|------|---------|---------|---------------|
| 変数 | hensū | variable | Variables (変 = change, 数 = number) |
| 結果 | kekka | result | Command result ($?) |
| 結 | ketsu / musubu | bind, tie, conclude | Bindings, results |
| 値 | chi / atai | value | Values |
| 型 | kata / gata | type, form | Type system |
| 文字 | moji | character, letter | Strings |
| 列 | retsu | row, sequence | Arrays |
| 空 | kū / kara | empty, void | Null/empty values |

---

## Files & Paths

| 漢字 | Reading | Meaning | Potential Use |
|------|---------|---------|---------------|
| 道 | michi / dō | path, way | File paths |
| 路 | ro / michi | road, path | Alternative for paths |
| 読 | doku / yomu | read | File reading |
| 書 | sho / kaku | write | File writing |
| 消 | shō / kesu | erase, delete | `rm` builtin |
| 複 | fuku | copy, duplicate | `cp` builtin |
| 移 | i / utsuru | move, transfer | `mv` builtin |

---

## Tools & Functions

| 漢字 | Reading | Meaning | Potential Use |
|------|---------|---------|---------------|
| 器 | ki / utsuwa | tool, vessel, container | Tools |
| 道具 | dōgu | tool, implement | Tool definitions |
| 関数 | kansū | function | User-defined tools |
| 命 | mei / inochi | command, order, life | Commands |
| 命令 | meirei | command, instruction | Command execution |
| 呼 | ko / yobu | call | Tool calls |

---

## Errors & Status

| 漢字 | Reading | Meaning | Potential Use |
|------|---------|---------|---------------|
| 成功 | seikō | success | Success status |
| 失敗 | shippai | failure | Failure status |
| 誤 | go / ayamari | error, mistake | Errors |
| 警告 | keikoku | warning | Warnings |
| 完了 | kanryō | complete | Completion status |
| 進 | shin / susumu | advance, progress | Progress indicators |

---

## Network & Communication

| 漢字 | Reading | Meaning | Potential Use |
|------|---------|---------|---------------|
| 接続 | setsuzoku | connection | MCP connections |
| 送 | sō / okuru | send | Sending data |
| 受 | ju / ukeru | receive | Receiving data |
| 通 | tsū / tōru | pass through, communicate | Communication |

---

## Fun Combinations

| 漢字 | Reading | Meaning | Notes |
|------|---------|---------|-------|
| 散集 | sanshū | scatter-gather | The signature feature! |
| 会流 | kairyū | meeting-flow | Could describe pipelines |
| 核心 | kakushin | core, kernel | Literally "core heart" |
| 並走 | heisō | parallel running | Parallel processes |
| 道標 | michishirube | signpost, guide | Help system? |

---

## Numbers (for counts/limits)

| 漢字 | Reading | Value |
|------|---------|-------|
| 一 | ichi | 1 |
| 二 | ni | 2 |
| 三 | san | 3 |
| 四 | shi / yon | 4 |
| 五 | go | 5 |
| 六 | roku | 6 |
| 七 | shichi / nana | 7 |
| 八 | hachi | 8 |
| 九 | kyū / ku | 9 |
| 十 | jū | 10 |
| 百 | hyaku | 100 |
| 千 | sen | 1000 |

---

## Learning Tips

1. **Radicals matter**: 会 contains 人 (person) + 云 (cloud/speak) — people coming together to speak
2. **Compounds build meaning**: 散 (scatter) + 集 (gather) = the complete parallel pattern
3. **On'yomi vs Kun'yomi**: Chinese readings (音読み) often appear in compounds; Japanese readings (訓読み) stand alone
4. **Practice writing**: Stroke order helps memory. 核 is: 木 (tree) + 亥 (pig/boar radical)

---

## REPL Prompt Ideas

```
会sh>           # Standard
会sh 散>        # During scatter
会sh 集>        # During gather
会sh 待>        # Waiting
会sh ⚡>        # Fast mode?
会sh 核>        # Kernel status
```

---

*This document will grow as we develop kaish. Add kanji that feel right for new features!*
