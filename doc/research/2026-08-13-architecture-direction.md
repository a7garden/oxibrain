# oxibrain 아키텍처 방향 분석 — 2026년 중반 레퍼런스 리뷰

> **Date:** 2026-08-13 · **Status:** Research memo (not authoritative)
> **Scope:** 외부 레퍼런스 14건 분석 + 현재 구현 실측 + 방향 권고
> **Authority:** 없음. **역사적 기록**이다.
> **후속:** 이 메모의 결론은 `doc/ARCHITECTURE.md` v2.0에 흡수되었고, 그쪽이 canonical이다.
> 충돌하면 **ARCHITECTURE.md가 맞다** — 그 문서는 레퍼런스 7종(graphiti 추가)을 소스 레벨로
> 읽고 코드베이스를 전수 측정한 뒤 작성됐다.
> **주의:** 이 메모의 `DESIGN §n` 참조는 전부 **DESIGN.md v1.0 기준**이다. v2.0에서 절 번호가
> 바뀌었으므로 그대로 따라가면 안 된다.

---

## 0. 결론 먼저

세 문장으로 요약하면:

1. **설계는 2026년 기준으로도 옳은 편에 서 있다.** assertion log + bi-temporal fold +
   deterministic reprojection은 이번에 본 어떤 시스템보다도 감사가능성과 복구력이 강하다.
   Zep/Graphiti 계열이 검증한 방향이고, mem0 계열의 destructive update를 거부한 D15는
   2026년 시점에도 유효하다.
2. **그런데 그 설계의 핵심 주장이 아직 한 번도 측정된 적이 없다.** 그리고 측정을 막고 있는
   것은 이론이 아니라 **임베딩 어댑터가 워크스페이스에 하나도 없다**는 사실이다.
   현재 `semantic` 모드는 1024차원 해싱 TF-IDF이고, 에이전트가 매 턴 호출하는
   `assemble_context`는 내부적으로 **lexical 단일 모드**로만 검색한다.
3. **2026년의 진짜 변화는 "무엇을 저장하는가"가 아니라 "에이전트가 그것을 어떻게 소비하는가"에서
   일어났다.** 컨텍스트를 **한 덩어리로 건네주는 것**에서 에이전트가 **직접 항해하는 것**으로,
   그리고 **쿼리 의존 검색**에서 **쿼리 독립 프로필 + 쿼리 의존 검색의 이중화**로.
   oxibrain은 이 두 가지를 담을 기반(entity/statement/belief/provenance)을 이미 다 갖고 있는데,
   **표면(surface)만 없다.**

권고 우선순위:

| # | 권고 | 성격 | 근거 |
|---|---|---|---|
| **R1** | 임베딩 어댑터 실체화 | **차단 요인** | 나머지 모든 측정의 전제 |
| **R2** | `assemble_context`를 제품의 중심으로 승격 + Profile 레이어 | 아키텍처 추가 | supermemory, Claude Memory |
| **R3** | 에이전트-네이티브 항해 표면 (`brief` / `navigate`) | 아키텍처 추가 | LLM-Wiki 논문, SMFS, Claude memory |
| **R4** | contextual chunk + cue anchor를 인덱스 층에 | 인덱스 개선 | Anthropic CR, Memora |
| **R5** | 요약에 불확실성 보존 | **설계 수정** | clawsouls 실험 (요약 단독은 성능을 *떨어뜨림*) |
| **R6** | 통제 베이스라인 평가 실행 | 프로세스 | MemDelta, LongMemEval-V2 |
| **R7** | 그래프를 "질의 구조 + 랭킹 신호" 이중 용도로 헤지 | 리스크 관리 | mem0 2026 리포트의 업계 후퇴 |
| **R8** | 명시적 기각 목록 유지 | 방어 | morphik, mem0, 클라우드 3사 |

---

## 1. 현재 위치 — 실측

문서가 주장하는 것이 아니라 **코드에서 확인한 것**만 적는다. (2026-08-13, `main` @ `cc584b7`)

**규모와 건강도**

- 10 crates, Rust 소스 약 20,934 LOC
- `cargo test --workspace` → **237 passed, 0 failed**
- git history 기준 M0–M6 전부 랜딩 (store, fold, resolution, retrieval, extraction,
  MCP 서버, 보안/토큰/감사/redaction, oxios importer, 데스크톱 UI)

설계 문서 대비 완성도는 상당히 높다. 그런데 **정확히 제품의 심장부에 구멍이 있다.**

**확인된 갭 (전부 코드 근거 있음)**

| # | 갭 | 근거 |
|---|---|---|
| G1 | `EmbeddingPort` 구현체가 **0개** | `grep "impl.*EmbeddingPort" crates/` → no match. DESIGN §15의 `oxibrain-embed-local` 크레이트는 존재하지 않음 |
| G2 | dense 벡터 경로가 죽어 있음 | `semantic_search`(`store/query.rs:278`)는 dense 분기에서 아무것도 하지 않고 주석만 남긴 채 TF-IDF로 폴백. `upsert_vector`는 **프로덕션 호출자 없음** (테스트에서만 호출) |
| G3 | `hybrid_query`가 `as_of` / `min_confidence`를 **읽지 않음** | `query.rs:339–506` 전체에 두 필드 참조 없음. 즉 하이브리드 검색에 시간축 필터가 걸리지 않는다 |
| G4 | `dropped`가 **항상 빈 배열** | `let dropped: Vec<DroppedItem> = Vec::new();` 이후 push 없음 → DESIGN §13.3이 약속한 `why --dropped`("버린 것을 계측하라")에 데이터가 없음 |
| G5 | `assemble_context`가 **lexical 단일 모드** | `store/context.rs`에서 `mode: QueryMode::Lexical`로 `hybrid_query` 호출 → 에이전트가 매 턴 부르는 함수에서 semantic/graph/community가 한 번도 안 돈다 |
| G6 | context 레이어 2개가 미구현 | `PinnedFacts` 비어 있음, `QueryNeighborhood`는 `// keeping M2 simple` 주석과 함께 스킵 |
| G7 | `RecallHints`가 사실상 무동작 | `is_session_start`/`topic_changed`가 `recent_limit`을 5→20으로 바꾸는 것이 전부. 커뮤니티 요약 포함 없음 |
| G8 | belief 렌더링이 주어를 버림 | `format!("... {predicate} {object_repr} ...")` — 주어가 문자 그대로 `...`이고 목적어는 raw entity id |
| G9 | `beliefs_as_of`가 transaction time을 무시 | `let _ = transaction_at; // M2` |
| G10 | 청킹 없음, 리랭킹 없음 | DESIGN §7.4의 "chunking with overlap" 미구현, rerank 관련 코드 0건 |
| G11 | eval이 자기 자신을 증명하지 않음 | `compute_metrics`가 `fabricated_entity_rate = 0.0`을 **하드코딩** ("구조적 보장"이라는 주석). 골든 코퍼스 파일 디스크에 없음 |

**이 목록을 한 문장으로 요약하면:**

> DESIGN.md가 "temporal KG가 flat vector memory를 이긴다"를 논지로 걸었는데,
> 현재 구현에는 **vector memory 쪽이 아예 존재하지 않아서** 그 비교 자체가 불가능하다.

이건 비난이 아니라 스케줄링의 결과다. M1을 "LLM 없는 결정론적 코어"로 잡은 D9는 옳은 결정이었고,
그 대가로 비결정적인 부분(임베딩, 추출 품질)이 뒤로 밀린 것이다. 다만 **지금이 그 빚을
갚아야 하는 시점**이라는 게 이 보고서의 첫 번째 결론이다.

---

## 2. 레퍼런스별 분석

각 항목: **무엇인가 → 핵심 아이디어 → oxibrain 시사점 → 판정**

### 2.1 Karpathy, "LLM Wiki" (gist) + *Retrieval as Reasoning* (arXiv:2605.25480)

**무엇** — 문서를 청크로 쪼개 매 쿼리마다 재합성하는 대신, LLM이 **상호링크된 마크다운 위키를
점진적으로 짓고 유지**한다. 3층: 원본 소스(불변) / 위키(LLM 유지) / 스키마(CLAUDE.md 류).
연산 3개: `ingest` / `query` / `lint`. 논문 쪽은 이걸 벤치마크로 검증했다 —
search·read·link-follow 툴 + "Error Book" 자기교정으로 HotpotQA / MuSiQue / 2WikiMultiHopQA에서
HippoRAG 2, LightRAG, GraphRAG 대비 **+2.0–8.1 F1**. ablation에서 **위키 구조 자체(−6.1~−7.0)보다
링크를 따라가는 반복 항해(−11.7~−13.8)가 두 배 더 기여**했다.

**시사점** — 이건 oxibrain에게 가장 중요한 레퍼런스다. 두 가지를 동시에 말하기 때문이다.

1. **좋은 소식:** oxibrain의 "누적되는 구조화 지식" 논지가 실증적으로 지지받았다.
   매번 재합성하는 RAG보다 컴파일된 구조가 낫다.
2. **불편한 소식:** 성능의 대부분은 *구조를 갖는 것*이 아니라 *에이전트가 그 구조를 항해하는 것*에서
   나왔다. oxibrain은 구조는 최고 수준으로 갖췄지만, 항해 표면이 없다. `traverse`가 있긴 하나
   그건 서브그래프를 반환하지 **읽을 수 있는 페이지와 따라갈 링크**를 주지 않는다.

그리고 **oxibrain이 LLM-Wiki보다 구조적으로 유리한 지점이 하나 있다.** LLM-Wiki는 마크다운
페이지가 시간이 지나며 어긋나기 때문에 `lint`와 Error Book으로 계속 보수해야 한다. 저자 본인이
"유지보수가 발표된 구현들이 간과하는 실제 운영 과제"라고 짚었다. oxibrain에서 페이지는
**fold의 렌더링**이므로 어긋날 수가 없다.

> **LLM-Wiki가 lint로 푸는 문제를 oxibrain은 reproject로 푼다.**

**판정: 채택 (R3).** 단, **위키를 파일로 물질화하지 않는다.** 그건 §1.4(에디터 아님)와 P1을
동시에 위반한다. 페이지는 저장되지 않고 렌더링된다.

### 2.2 supermemory

**무엇** — LongMemEval / LoCoMo / ConvoMem 1위를 주장하는 메모리 레이어. LongMemEval에서
**Recall@15 95%, 컨텍스트 99.4% 감소**. 카테고리별로 knowledge update 99%, temporal reasoning 91%.
Cloudflare Workers + Postgres 기반 클라우드가 본체이고, 로컬 단일 바이너리 모드도 제공한다
(`Xenova/bge-base-en-v1.5` 로컬 임베딩, Ollama 옵션).

세 가지가 눈에 띈다.

**(a) User Profile — 쿼리 독립 컨텍스트.** 문서에 있는 예시가 정확히 문제를 짚는다:
사용자가 온보딩 때 "내 이름은 Dhravya로 불러줘"라고 한 번 말했다. 나중에 "일본 여행 계획 짜줘"라는
쿼리가 오면, 이 선호는 **벡터 공간에서 여행과 아무 관련이 없으므로 절대 검색되지 않는다.**
검색은 정상 동작한 것이고, 애초에 검색으로 해결할 문제가 아니었던 것이다.
그래서 프로필은 static(안정적 사실) / dynamic(최근 활동) 두 층으로 나뉘어 **모든 프롬프트에
붙어 다닌다.** 검색 3–5회 왕복 대신 1회 호출, 200–500ms 대신 50–100ms.

**(b) Dreaming.** 문서 인덱싱(`done`)과 그래프 진입(메모리 생성)을 **분리된 2단계**로 둔다.
oxibrain의 consolidation과 같은 자리인데, supermemory는 이걸 쓰기 경로에서 완전히 떼어냈다.

**(c) SMFS — "Memory your agent can grep".** 메모리 컨테이너를 실제 디렉토리로 마운트하고
에이전트가 `ls`/`cat`/`grep`으로 읽는다. `grep`은 기본값이 **시맨틱**이고, 플래그를 주면 진짜 grep으로
폴백한다. 루트에 **가상 `profile.md`** 가 있어서 트리를 다 걷지 않고 한 번에 요약을 볼 수 있다.
Claude에서 토큰 3.0×, Codex에서 1.75× 절감.

**시사점** — 프로필은 oxibrain에 **새 저장소가 필요 없다.** oxibrain 용어로 프로필은
`Functional` cardinality + 높은 confidence + 지정된 predicate 집합에 대한 **상시 질의(standing query)**를
렌더링해 캐시한 **Derived view**다. P1/P2와 완전히 정합한다. 그리고 oxibrain은 supermemory가
못 하는 걸 할 수 있다 — **프로필의 각 줄에 provenance와 validity interval을 붙일 수 있다.**

SMFS의 파일시스템 은유 자체는 채택할 필요 없다(§1.4 위반, 그리고 우리는 MCP를 이미 갖고 있다).
채택할 것은 그 **인체공학**이다: 도구 개수를 늘리지 말고, **탐색 가능한 표면**을 주고,
**`profile.md` 같은 단일 진입점**을 둔다.

**판정: 프로필 채택 (R2), SMFS 인체공학 채택 (R3), 파일시스템/클라우드 기각.**

### 2.3 Microsoft Memora

**무엇** — "harmonic memory representation": 저장하는 것과 조직·접근하는 것을 분리한다.
각 메모리 항목이 3부분이다.

- **Memory value (인덱싱 안 함)** — 손실 없는 전체 정보
- **Primary abstraction (인덱싱함)** — 1:1 요약. 업데이트/집계의 canonical 단위
- **Cue anchors (인덱싱함)** — 2–5단어짜리 다중 시맨틱 진입점, N:N

리포지토리의 cue 생성 프롬프트가 구체적이다: `[행위자] [개념]`, `[행위자] [행동/사건]`,
`[도메인] [주제]` 같은 패턴으로 **primary index가 못 잡는 관점만** 0–3개 생성.
저장은 ChromaDB, 검색은 semantic / prompted(LLM 다단계) / hybrid(BM25) / GRPO(RL, 실험).

**시사점** — 이건 oxibrain이 **이미 절반을 하고 있는데 절반만 하고 있는** 아이디어다.
`Statement`가 사실상 primary abstraction이고(원문은 episode에 온전히 남아 있으니 value/abstraction
분리도 성립), 없는 것은 **cue anchor 층**이다.

oxibrain에서 "무엇을 인덱싱하는가"를 보면: 에피소드 전문 FTS + statement 렌더링 + 엔티티.
Memora가 지적하는 문제는 이거다 — **원문을 그대로 인덱싱하면 임베딩이 흐려지고,
추상만 인덱싱하면 진입점이 하나뿐이라 놓친다.** 후자가 정확히 oxibrain의 현재 상태다.

**판정: 부분 채택 (R4).** 단 cue는 LLM 출력이므로 `summaries`처럼 extractor id로 캐시해야
D5(Derived는 terminal)와 결정성이 유지된다. 그리고 **결정론적 cue를 먼저 짜보는 게 낫다** —
statement가 이미 `(주어, 술어, 목적어)`를 갖고 있으니 "Melanie 목걸이 의미" 같은 cue는
LLM 없이 registry에서 생성 가능하다. LLM cue는 eval이 갭을 보여줄 때만 산다.

### 2.4 Stash (alash3al)

**무엇** — 가장 가까운 구조적 아날로그. 원시 관측을 **episode**로 저장하고 배경 파이프라인이
facts / relationships / patterns / goals / **failures** / hypotheses로 승격시킨다.
PostgreSQL + pgvector, namespace + 계층 경로, MCP 28개 도구, 5분 주기 "research loop"로
자율 학습, Apache 2.0.

**시사점** — 두 가지가 흥미롭다.

첫째, **승격 타입에 `failures`와 `hypotheses`가 있다.** oxibrain의 predicate registry
`core/v1`은 Person/Organization/Project/... 로 세계를 서술하는 온톨로지인데, 에이전트 메모리로
쓰이려면 **"뭐가 안 됐는지", "뭘 아직 모르는지"** 를 표현할 수 있어야 한다. LongMemEval-V2가
새로 추가한 "environment gotchas" 능력과 정확히 같은 것을 가리킨다(2.13 참조).

둘째, **MCP 도구 28개**는 반면교사다. oxibrain은 §12.2에서 13개인데, 이것도 많다. 2026년의
방향은 도구를 늘리는 게 아니라 **적은 도구로 항해 가능하게 만드는 것**이다(2.1, 2.2 참조).

**판정: registry에 negative/uncertain predicate 계열 추가 검토 (R5와 함께). 도구 개수는 늘리지 않는다.**

### 2.5 pingcap/autoflow

**무엇** — GraphRAG + 벡터를 TiDB Serverless 하나에 얹은 Perplexity 스타일 대화형 지식베이스.
FastAPI + Next.js + LlamaIndex + **DSPy**. 사이트맵 크롤러, 임베더블 JS 위젯.

**시사점** — 아키텍처적으로는 oxibrain과 겹치는 게 적다(서버 사이드, Python, 외부 DB —
§2 non-goals 정면 위반). 가져갈 것은 하나: **DSPy 식 "프롬프트를 손으로 쓰지 말고 컴파일하라"**.

oxibrain은 이미 이걸 다른 경로로 하고 있다 — 추출 스키마를 registry에서 **생성**한다(P4).
그런데 프롬프트 텍스트 자체(`build_extraction_prompt`)는 여전히 수작업이고, 개선 루프가 없다.
DSPy가 파는 것은 "few-shot 예시를 측정 기반으로 자동 선택"인데, oxibrain에는 그걸 돌릴 재료가
이미 있다: `extraction_failures` 테이블과 골든 코퍼스. **실패 사례를 few-shot으로 재활용하는
루프**는 저렴하고 D8(registry minor 버전은 캐시를 무효화하지 않음)과도 정합한다.

**판정: DSPy 자체는 기각(Python). "실패를 few-shot으로 되먹임" 아이디어만 채택 (R9, 낮은 우선순위).**

### 2.6 morphik-core

**무엇** — 시각적으로 풍부한 문서(PDF, 차트, 다이어그램)를 **ColPali**로 처리하는 멀티모달 RAG.
Python + 일부 Rust(`morphik_rust`), MCP 지원, BSL 1.1 라이선스(4년 후 Apache 2.0).

**시사점** — DESIGN §2가 "OCR / 미디어 이해"를 명시적 non-goal로 박아둔 것이 옳았다는 확인.
ColPali는 페이지를 이미지로 임베딩하는 접근이라 사실상 GPU + Python 파이프라인을 요구한다.
단일 Rust 바이너리 제약과 양립 불가능하다.

**다만 하나의 압력은 인정해야 한다:** 사용자의 실제 vault에는 PDF와 스크린샷이 있다.
답은 §2에 이미 있다 — **connector가 사전 전사(pre-transcribe)한다.** 이걸 로드맵에 명시적으로
남겨두는 게 좋겠다(markdown connector 옆에 `pdf-text` connector, 텍스트 추출만).

**판정: 기각 (non-goal 재확인). connector 레벨 텍스트 추출로 압력 해소.**

### 2.7 refactoringhq/tolaria

**무엇** — Tauri 2 + React + Rust로 만든 마크다운 지식베이스 데스크톱 앱. git 저장소가 진실의
원천("full version history, any git remote, zero dependency on Tolaria servers"), 10,000+ 노트,
키보드 중심, MCP 서버 내장, AGPL-3.0, 커밋 3,600+.

**시사점** — oxibrain 자체보다 **oximemo와 ECOSYSTEM §3.1 경계선**에 대한 레퍼런스다.
tolaria는 정확히 "oximemo가 노트 모드를 갖추면 되는 것"의 완성형이고, 기술 스택도 같다
(Tauri 2 + Rust). 그리고 **files-first + git + MCP**라는 조합이 실제로 팔린다는 증거다.

여기서 나오는 질문은 하나: **oximemo가 tolaria가 되어야 하는가, 아니면 oxibrain이 tolaria 같은
앱들의 백엔드가 되어야 하는가?** ECOSYSTEM.md는 전자를 택했다(§3.1). 그런데 tolaria처럼
이미 존재하는 잘 만들어진 앱이 MCP 서버를 갖고 있다면, oxibrain 입장에서는
**"tolaria의 vault를 connector로 읽는다"**가 훨씬 싼 사용자 획득 경로다.

**판정: oximemo 로드맵 재검토 신호로 기록. oxibrain 본체 변경 없음.**
DESIGN §1.4의 "에디터 불가지론이 Obsidian 사용자를 고객으로 만든다"는 논리가 tolaria에도 그대로 적용된다.

### 2.8 Anthropic — Contextual Retrieval

**무엇** — 각 청크를 임베딩하기 **전에** 문서 전체 맥락에서 뽑은 짧은 설명을 앞에 붙인다.
contextual embedding만으로 검색 실패 **−35%**, contextual BM25까지 더하면 **−49%**,
리랭킹까지 더하면 **−67%**. 프롬프트 상위 20청크 권장. prompt caching으로 비용을 눌렀다.

**시사점** — oxibrain의 인덱스 층에 가장 직접적으로 적용 가능한 레퍼런스이고,
동시에 **G10(청킹 없음)**을 가리킨다. 지금 oxibrain은 에피소드를 통째로 인덱싱한다.
긴 노트나 대화 로그에서는 이게 정확히 Anthropic이 지적한 실패 모드를 만든다.

그런데 oxibrain에는 **남들이 LLM을 불러서 만들어야 하는 맥락이 이미 구조화되어 있다.**
어떤 청크에 대해 그 span에서 추출된 mention/statement, 그 에피소드의 `occurred_at`,
`SourceRef`, 소속 community를 알고 있다. 즉:

> contextual prefix를 **LLM 호출 없이 projection에서 결정론적으로 생성할 수 있다.**

이건 결정성(P1)을 지키면서 공짜로 얻는 개선이다. LLM 생성 맥락은 그 위의 유료 업그레이드로
남겨두고, `extractions`처럼 캐시하면 된다.

**판정: 채택 (R4). 결정론적 prefix 먼저, LLM prefix는 eval이 요구할 때.**

### 2.9 Meta REFRAG

**무엇** — 청크를 경량 인코더로 압축 임베딩화해서 LLM이 직접 소비하게 하고, **RL 정책망이
중요한 청크만 골라 전체 토큰으로 확장**한다. KV 캐시·어텐션 연산·TTFT를 크게 줄여 응답 **30×**.

**시사점** — RL 정책망이나 압축 임베딩 자체는 oxibrain 범위 밖이다(모델 학습 필요).
하지만 **아이디어의 텍스트 레벨 등가물은 정확히 `assemble_context`가 해야 할 일**이다:

> 대부분은 압축 표현(belief 한 줄)으로 보내고, 상위 소수만 원문으로 확장한다.

현재 `assemble_context`는 정확히 반대를 한다 — 최근 에피소드를 **원문 그대로** 예산이 찰 때까지
쏟아붓는다(`store/context.rs`). 그리고 belief 쪽은 주어가 `...`인 쓸모없는 한 줄이다(G8).
즉 압축해야 할 걸 확장하고, 확장해야 할 걸 망가뜨리고 있다.

**판정: 아이디어만 채택 (R2). RL 없이 salience/confidence 기반 결정론적 확장 정책으로.**

### 2.10 Claude Memory (Anthropic) + "Claude의 메모리 아키텍처는 ChatGPT의 정반대" (shloked)

**무엇** — Claude는 **명시적 호출**로 과거 대화를 검색한다(`conversation_search` 키워드,
`recent_chats` 시간 기반). 그리고 요약이 아니라 **실제 과거 대화**를 반환한다.
ChatGPT는 정반대로 세션 시작 시 프로필과 이력을 **자동 로드**한다.
분석은 이걸 제품 철학의 차이로 본다 — 개발자/전문가 대상의 투명성·통제·프라이버시 vs
일반 소비자 대상의 매끄러운 개인화. 그리고 양쪽 다 하이브리드로 수렴 중이라고 관측한다.
Anthropic의 Memory 제품 자체는 **프로젝트별 메모리 분리**, **incognito 채팅**,
**사용자가 보고·편집·삭제 가능**, 조직 관리자 비활성화 가능, import/export를 갖췄다.

**시사점** — oxibrain은 **이미 두 축을 다 갖고 있는데 구분해서 팔지 않고 있다.**

| 축 | Claude형 (명시적) | ChatGPT형 (자동) |
|---|---|---|
| oxibrain 표면 | `search`, `why`, `timeline`, `traverse` | `recall` = `assemble_context` |
| 반환물 | 원문 에피소드 + provenance | 팩된 컨텍스트 |
| 상태 | 대체로 구현됨 | **G5–G7로 사실상 미구현** |

그리고 두 가지가 빠져 있다:

1. **Incognito.** "이 세션은 ledger에 쓰지 않는다"는 개념이 oxibrain에 없다. P1이
   "ledger가 유일한 durable write path"라고 못박은 것과 정면으로 부딪히는 것처럼 보이지만
   그렇지 않다 — **애초에 episode를 만들지 않는 것**은 P1을 위반하지 않는다.
   scope/capability 층에 `Ingest` 미부여로도 표현 가능하지만, 사용자가 이해하는 단위는
   "이번 대화는 기억하지 마"이므로 **세션 플래그로 노출**되어야 한다.
2. **사용자가 기억을 보고 편집.** oxibrain에는 `review`/`contradictions`는 있는데
   "브레인이 나에 대해 아는 것 전부 보여줘"가 없다. 이건 R2의 Profile이 그대로 답이 된다.

Anthropic의 **프로젝트별 메모리 분리**는 oxibrain의 space와 정확히 같은 개념이며,
ECOSYSTEM C2("space는 프라이버시 경계이지 앱 경계가 아니다")가 옳았다는 확인이다.

**판정: 이중 축을 MCP 표면에서 명시화 (R2/R3). incognito 세션 플래그 추가 (R10, 작음).**

### 2.11 "AI 에이전트 메모리 실험: 요약된 지식이 오히려 성능을 떨어뜨린다" (clawsouls)

**무엇** — Claude로 4가지 메모리 구성을 두고 같은 프로젝트에 대해 20개 질문. 5점 만점:

| 구성 | 점수 |
|---|---|
| 하이브리드 (경험 로그 + 요약) | **4.95** |
| 원시 경험 로그만 | 4.55 |
| **메모리 없음** | **3.30** |
| **정리된 요약만** | **2.65** |

핵심은 "overconfidence effect": **깔끔한 요약은 에이전트에게 근거 없는 확신을 주고
모른다고 말하는 능력을 떨어뜨린다.** 원시 로그는 불확실성의 흔적(실패, 미해결)을 보존해서
더 정직한 추론을 가능하게 한다.

**시사점** — 이건 이번 레퍼런스 중 **oxibrain 설계에 가장 불편한 결과**다.

DESIGN §10(consolidation)과 §9.4(community summaries)는 "요약이 salience에서 이기기 때문에
검색이 요약을 선호한다"고 명시한다. 이 실험대로면 그건 **메모리 없음보다 나쁜 구성(2.65 < 3.30)**을
기본값으로 만드는 길이다.

oxibrain에는 해독제가 이미 있는데 **요약 경로에 연결되어 있지 않다**: `confidence`,
`support`(affirm/deny 카운트, distinct episode 수), `BeliefStatus::Contradicted`, provenance.
즉 불확실성을 표현할 어휘를 다 갖고 있으면서 요약 텍스트에는 안 넣고 있다.

두 가지 수정이 필요하다:

1. **Derived episode는 구조화된 불확실성 블록을 동반해야 한다** — 무엇이 모순되었는지,
   무엇이 단일 에피소드 지지뿐인지, 무엇이 오래됐는지.
2. **`assemble_context`는 요약 단독을 절대 보내지 않는다** — 항상 요약 + 지지 에피소드 표본
   (= 실험의 4.95 구성). 이건 R2와 같은 작업이다.

**판정: 채택. DESIGN §10 / §9.4 수정 필요 (R5). 이번 리뷰에서 나온 유일한 "설계 수정" 항목.**

### 2.12 RAG 기술 서베이 (discuss.pytorch.kr, 1/2편)

**무엇** — Naive / Advanced / Modular RAG 3단계 분류. Advanced는 pre-retrieval(인덱싱 최적화,
메타데이터, 임베딩 파인튜닝) / post-retrieval(리랭킹, 프롬프트 압축) / 파이프라인 최적화
(하이브리드 검색, 재귀 검색, step-back, subquery, HyDE). Modular는 search/memory/validation을
교체 가능한 모듈로.

**시사점** — 진단 도구로 쓰면, **oxibrain의 검색 스택은 현재 "Naive + 구조"에 가깝다.**
하이브리드 융합(RRF)은 Advanced 요소지만, pre-retrieval(청킹·contextual·메타데이터 필터)과
post-retrieval(리랭킹·압축)이 통째로 비어 있다(G10). 지식 모델의 정교함과 검색 파이프라인의
정교함 사이에 큰 낙차가 있다.

**판정: R4에 흡수.** 특히 **리랭킹**은 Anthropic 수치(−49% → −67%)가 보여주듯 단일 항목으로
가장 큰 개선이며, 로컬에서도 작은 cross-encoder로 가능하다.

### 2.13 2026년 벤치마크 지형 — mem0 리포트, MemDelta, LongMemEval-V2

**mem0 State of AI Agent Memory 2026** — LoCoMo 92.5(6,956 tok/q), LongMemEval 94.4(6,787 tok/q),
BEAM-1M 64.1, BEAM-10M 48.6. 최대 개선은 temporal reasoning +29.6, multi-hop +23.1.
아키텍처 트렌드로 **멀티 신호 검색(시맨틱+BM25+엔티티 링킹 융합)**, **비동기 기본 쓰기**를 꼽는다.
그리고 주목할 문장 하나:

> *"Entity relationships shifted from queryable graph structures to retrieval ranking signals."*

즉 업계는 **그래프를 질의 구조에서 랭킹 신호로 후퇴**시키고 있다. oxibrain은 정반대 베팅이다.
미해결 문제로는 temporal abstraction(1M→10M에서 25% 하락), cross-session identity,
**memory staleness**("높은 관련도의 사실이 상황 변화 후 자신 있게 틀린 것이 된다"), 프라이버시,
그리고 **벤치마크 점수가 실제 워크로드 성능을 예측하지 못한다**는 점.

**MemDelta (arXiv:2606.29914)** — 에이전트 메모리 평가의 숨은 교란변수를 지적한다.
아키텍처 변형, 구현 디테일(청킹·검색·통합 방식), 부적절한 베이스라인이 뒤섞여 있어서
"메모리 능력"과 "시스템 설계"를 분리할 수 없다는 것. 결론이 뼈아프다:

> **제대로 통제하면 정교한 메모리 시스템의 이득 상당수가 사라지고, 단순 베이스라인이 종종
> 대등하거나 더 낫다.**

**LongMemEval-V2 (arXiv:2605.12493)** — 451문항, 최대 500 trajectory / 115M 토큰,
대화가 아니라 **웹 에이전트 궤적**. 새 5능력: static state recall, dynamic state tracking,
workflow knowledge, **environment gotchas**, **premise awareness**. 최고 성능
AgentRunbook-C가 72.5%(쿼리당 ~108초), 단순 RAG는 57.8%(26.9초), 궤적 없는 프런티어 LLM은 1.3%.
gotchas 카테고리는 최고 시스템도 48.3%.

**시사점** — 세 가지가 동시에 나온다.

1. **oxibrain의 §14 목표(LongMemEval ≥85)는 2026년 기준으로 이미 SOTA 대비 낮다**(94.4).
   하지만 그건 문제가 아니다 — 문제는 **토큰 예산과 설정을 명시하지 않으면 비교 불가**라는 점인데
   DESIGN §14.1이 이미 그렇게 규정해 두었다. 이건 잘 되어 있다.
2. **MemDelta는 §14를 다시 쓰게 만든다.** "우리 시스템이 85점"이 아니라
   **"통제된 베이스라인 대비 델타"** 가 보고 단위가 되어야 한다. 그리고 oxibrain은
   이 실험을 하기에 이례적으로 좋은 위치에 있다 — ledger가 있으니 같은 코퍼스에 대해
   (a) full-context, (b) BM25+dense 청크만(KG 없음), (c) oxibrain 전체를 **동일 데이터로**
   돌릴 수 있다.
3. **LongMemEval-V2는 oxios가 있어야 의미 있는 벤치마크다.** agent trajectory가 입력이고
   "gotchas"는 정확히 2.4에서 언급한 failures/hypotheses 표현력을 요구한다.
   v1에서 쫓을 대상은 아니지만, **registry가 부정적/불확실 지식을 표현할 수 있게 해두면**
   나중에 열리는 문이다.

**판정: §14 개정 (R6). 그래프 베팅 헤지 (R7).**

---

## 3. 종합 — 2026년의 다섯 가지 방향

레퍼런스를 가로질러 읽으면 다섯 개의 축이 반복된다.

**D1. 검색(retrieval) → 항해(navigation).**
LLM-Wiki 논문의 ablation이 가장 명확한 증거다(항해가 구조보다 2배 기여). SMFS가 파일시스템을
고른 이유도 같다("모든 모델이 이미 파일시스템을 안다"). Claude의 `conversation_search`도
자동 주입이 아니라 에이전트가 부르는 도구다. **컨텍스트를 잘 싸서 건네는 것보다,
에이전트가 스스로 파고들 수 있게 하는 것이 이긴다.**

**D2. 인덱스와 콘텐츠의 분리.**
Memora(value / abstraction / cue), Anthropic contextual retrieval(청크 ≠ 임베딩되는 텍스트),
supermemory(contextual chunking). 공통 명제: **저장 단위와 검색 진입점은 다른 것이며,
진입점은 여러 개여야 한다.**

**D3. 쿼리 독립 컨텍스트의 재발견.**
supermemory profile, ChatGPT 자동 로드, Anthropic Memory. "이름을 뭐라 부를지"는 어떤 쿼리와도
시맨틱하게 가깝지 않다. **순수 검색 아키텍처에는 구조적 사각지대가 있고,
프로필은 그 사각지대를 메우는 별개의 메커니즘이다.**

**D4. 요약은 위험하다.**
clawsouls 실험(요약 단독 2.65 < 무메모리 3.30). mem0의 "memory staleness — 자신 있게 틀림".
LongMemEval-V2의 gotchas 48.3%. **압축은 불확실성을 먼저 잃는다.**

**D5. 평가 신뢰의 위기.**
MemDelta(통제하면 이득이 사라진다), mem0 자신도 "벤치마크 점수가 실제 워크로드를 예측하지 못함"을
미해결 문제로 인정. **숫자를 발표하는 것보다 통제된 델타를 발표하는 것이 정직하다.**

**그리고 하나의 역류:** D6 — **그래프의 후퇴.** 업계는 그래프를 질의 구조에서 랭킹 신호로
내리고 있다. oxibrain은 반대로 간다. 이건 틀린 베팅이 아니라 **다른 목적함수**다 —
랭킹 신호는 "왜 이걸 믿는가", "언제부터 참이었나", "누가 이걸 주장했나"에 답할 수 없다.
다만 **베팅인 것은 맞으므로 헤지가 필요하다**(R7).

---

## 4. 트레이드오프

| 축 | Flat vector (mem0류) | Temporal KG (oxibrain, Zep) | LLM-Wiki / 파일 | 프로필 (supermemory) |
|---|---|---|---|---|
| 구축 비용 | 낮음 | **높음** (LLM 추출) | 중간 | 낮음 |
| 쿼리 지연 | 낮음 | 중간 (조인) | **높음** (다단계 항해) | **매우 낮음** (50ms) |
| 감사가능성 | 없음 | **완전** | 부분 (링크) | 없음 |
| 시간 추론 | 약함 | **강함** | 약함 | 약함 |
| 모순 처리 | 덮어씀 | **보존+표면화** | lint 필요 | 덮어씀 |
| 잘못된 추출 복구 | 재구축 | **재투영 (무료)** | 재작성 | 재구축 |
| 쿼리 독립 사실 | 놓침 | 놓침 (현재) | 가능 (index 페이지) | **핵심 기능** |
| 멀티홉 | 약함 | 강함 | **가장 강함** | 없음 |
| 결정성 | N/A | **byte-identical** | 없음 | 없음 |
| 온보딩 | 즉시 | 모델 필요 | 모델 필요 | 즉시 |

읽는 법: **oxibrain의 열이 이기는 칸(감사가능성, 시간, 모순, 복구, 결정성)은 전부
"신뢰"에 관한 것이고, 지는 칸(비용, 지연, 온보딩, 쿼리 독립)은 전부 "경험"에 관한 것이다.**

이건 우연이 아니라 설계 선택의 논리적 귀결이고, **방향은 명확하다: 신뢰 쪽 우위를 지키면서
경험 쪽 갭을 메운다.** 반대 방향(경험을 위해 신뢰를 포기)은 이미 5개 회사가 하고 있고
그쪽에서 이길 이유가 없다.

---

## 5. 권고

### R1 — 임베딩 어댑터 실체화 · **차단 요인 · 최우선**

**왜 이게 1번인가:** R2~R7 전부가 "측정해서 결정한다"를 전제로 하는데, 임베딩이 없으면
semantic 모드가 해싱 TF-IDF라서 어떤 비교도 무의미하다. 그리고 §8.2의 "임베딩은 이름 매칭에서
2차 신호"라는 주장 자체가 검증된 적이 없다.

**할 것**

- `oxibrain-embed-local` 크레이트 생성 (DESIGN §15에 이미 계획되어 있음)
  - 어댑터 A: 로컬 dense (aarch64 GGUF 또는 ONNX; bge-small 급). 첫 `ingest` 시 다운로드
  - 어댑터 B: `oxibrain-embed-http` (OpenAI / Voyage), feature gate
  - TF-IDF는 남기되 **`semantic`이라 부르지 않는다** — `lexical-vector` 등으로 정직하게 개명
- `upsert_vector`를 projection 단계에 연결 (현재 호출자 없음, G2)
- `semantic_search`의 dense 분기를 실제로 구현 (쿼리 임베딩 → sqlite-vec KNN)

**설계 결정 하나가 새로 필요하다 (DESIGN.md에 없음):**

임베딩 float은 BLAS/스레드/하드웨어에 따라 비트 단위로 재현되지 않는다. 그런데 §14.3은
reprojection이 **byte-identical**이어야 한다고 못박는다. 셋 중 하나를 골라야 한다:

1. 벡터 테이블을 byte-identical 검증 **대상에서 제외**하고, 그 사실을 P1의 명시적 예외로 기록
2. 저장 전 **양자화**(int8 등)해서 재현 가능하게 만듦 — 검색 품질을 약간 희생
3. 벡터를 별도 zone으로 승격해 "projection이지만 결정론적이지 않음"을 타입 레벨에서 표현

**권고: 1번.** 예외를 인정하고 문서화하는 것이 가장 정직하고, 벡터는 어차피 순위에만 영향을
주고 belief에는 영향을 주지 않으므로(§9.2) P1의 정신을 해치지 않는다. 다만 **DESIGN §5.1의
zone 표와 §14.3에 명시적으로 써야 한다.** 지금 쓰지 않으면 나중에 테스트가 깨질 때
"버그인가 설계인가"를 다시 논쟁하게 된다.

### R2 — `assemble_context`를 제품의 중심으로 승격 + Profile 레이어

현재 이 함수는 리포지토리에서 가장 약한 코드인데, DESIGN §9.5와 ECOSYSTEM 양쪽에서
**"oxios가 자기 메모리 코드를 지울 수 있게 하는 단 하나의 함수"** 라고 선언된 함수다.

**할 것**

1. `QueryMode::Lexical` → `Hybrid` (G5). 한 줄이지만 영향이 가장 크다.
2. `QueryNeighborhood` 레이어 실제 구현 (G6) — 이미 `load_adjacency` / `bfs`가 있다.
3. `render_belief` 수정 (G8) — 주어를 넣고, entity id 대신 canonical key를 렌더링하고,
   validity interval과 support를 붙인다.
4. **REFRAG식 확장 정책** (2.9): 대부분은 belief 한 줄, 상위 k개만 원문 확장.
   현재는 정확히 반대. RL 없이 `salience × confidence × 최근성`으로 결정론적으로.
5. **`dropped` 채우기** (G4) — §13.3이 이미 약속한 것. 이게 있어야 `why --dropped`가 산다.
6. **`RecallHints`를 실제로 동작시키기** (G7) — 세션 시작 시 프로필 + 커뮤니티 요약 포함.

**새로 추가: Profile 레이어 (D3).**

```
LayerKind::Profile   ← 신규, 항상 첫 번째, 쿼리와 무관
```

**oxibrain에서 프로필은 새 저장소가 아니다.** 다음 조건을 만족하는 belief에 대한
**상시 질의를 렌더링해 캐시한 Derived view**다:

- subject가 space의 "자기(self)" 엔티티이거나 `pinned` 표시된 엔티티
- predicate가 registry에서 `profile_relevant` 플래그를 가짐 (registry 확장, minor 버전 → 캐시 무효화 없음, D8)
- `BeliefStatus::Active` 이고 confidence ≥ 임계값
- static / dynamic 분할은 predicate의 `temporality`가 이미 제공한다 (`Static` vs `Interval`)

**oxibrain이 supermemory보다 나을 수 있는 지점:** 프로필의 모든 줄에
provenance와 validity interval을 붙일 수 있고, 모순된 줄은 모순으로 표시할 수 있다.
"이 브레인이 나에 대해 아는 것"이 **감사 가능한 목록**이 된다 (2.10의 Anthropic Memory
"보고 편집할 수 있다"에 대한 답이기도 하다).

### R3 — 에이전트-네이티브 항해 표면 (`brief` / `navigate`)

**D1에 대한 답이자, 이번 리뷰에서 나온 가장 큰 신규 기회.**

**할 것 — 도구 2개 추가 (13 → 15, 그 이상은 안 됨):**

```
brief(target: entity | topic | space, depth)  → 마크다운 "페이지"
navigate(from, link_id)                        → 다음 페이지
```

`brief(entity)`가 렌더링하는 것 (전부 이미 존재하는 데이터):

- 정체성: canonical key, aliases, type, 첫 등장 에피소드
- 현재 belief: validity interval + confidence + support 카운트
- **모순**: 양쪽 provenance와 함께
- **불확실성**: 단일 에피소드 지지, 오래된 belief, 최근 변경 (R5와 연결)
- 이웃: 상위 N개, **각각이 따라갈 수 있는 링크**
- 타임라인 하이라이트: 변화 지점
- 출처: 에피소드 참조

**핵심 원칙 — 페이지는 저장되지 않고 렌더링된다.**

이게 §1.4(에디터 아님)와 P1을 동시에 지키는 방법이고, 동시에 **LLM-Wiki 대비 구조적 우위**다.
LLM-Wiki는 마크다운 페이지가 어긋나기 때문에 lint와 Error Book으로 계속 보수해야 한다.
oxibrain의 페이지는 fold의 함수이므로 어긋날 수가 없다.

> **LLM-Wiki가 lint로 푸는 문제를 oxibrain은 reproject로 푼다.**

이건 마케팅 문구가 아니라 실제로 테스트 가능한 주장이다 — 같은 ledger에서 `brief`를 두 번
호출하면 (캐시된 요약 텍스트를 제외하고) 같은 페이지가 나와야 한다.

**CLI 대응:** `oxibrain page <entity>` — §12.4의 CLI-first 원칙과 정합.

### R4 — 인덱스 층 현대화: contextual chunk + 리랭킹 + (조건부) cue

**D2에 대한 답이자 G10 해소.**

**단계 1 — 결정론적 contextual chunk (LLM 비용 0):**

에피소드를 오버랩 청킹하고(§7.4가 이미 규정), 각 청크를 임베딩/인덱싱하기 전에
**projection에서 뽑은 prefix**를 붙인다:

```
[2026-03-14 · Note: meeting.md · 언급: Alice(Person), ProjectX(Project) · 커뮤니티: infra]
<원문 청크>
```

이 정보는 전부 이미 있다 (mention span, `occurred_at`, `SourceRef`, community).
LLM 호출이 없으므로 결정론적이고 무료다. Anthropic이 LLM으로 만드는 것을
oxibrain은 구조에서 얻는다 — **이건 KG를 갖고 있어서 생기는 실질적 배당이다.**

**단계 2 — 리랭킹:**

Anthropic 수치상 단일 항목 최대 개선(−49% → −67%). 로컬 소형 cross-encoder 또는
LLM 리랭커를 `RerankPort`로. `LlmPort`/`EmbeddingPort`와 같은 패턴.

**단계 3 — cue anchor (조건부):**

Memora식 cue를 도입하되, **결정론적 cue를 먼저 시도한다.** statement가 이미
`(주어, 술어, 목적어)`를 갖고 있으므로 registry에서 cue 문자열을 생성할 수 있다.
LLM cue는 eval이 결정론적 cue로 못 메우는 갭을 보여줄 때만. LLM cue를 쓴다면
`summaries`처럼 extractor id로 캐시 (D5 준수).

### R5 — 요약에 불확실성 보존 · **설계 수정 필요**

**D4에 대한 답. 이번 리뷰에서 나온 유일한 "DESIGN.md가 틀렸을 수 있다" 항목.**

**문제:** DESIGN §10과 §9.4는 요약이 salience에서 이겨 검색이 요약을 선호하도록 설계한다.
clawsouls 실험대로면 이건 **무메모리보다 나쁜 구성(2.65 < 3.30)**을 기본값으로 만든다.

**수정안 — DESIGN §10에 추가할 것:**

1. **Derived episode는 불확실성 블록을 동반한다.** 요약 텍스트 옆에 구조화된 필드:
   `contradicted: [...]`, `single_source: [...]`, `stale_since: [...]`, `open: [...]`.
   이건 LLM이 생성하는 게 아니라 **fold에서 결정론적으로 계산된다** — 즉 캐시 무효화 대상이 아니고,
   요약 텍스트가 오래돼도 불확실성 블록은 항상 최신이다.
2. **`assemble_context`는 요약 단독을 절대 보내지 않는다.** 항상 요약 + 지지 에피소드 표본
   (실험의 4.95 구성 재현). R2와 같은 작업.
3. **registry에 부정/불확실 표현력 추가 검토** — Stash의 failures/hypotheses,
   LongMemEval-V2의 gotchas/premise awareness가 같은 것을 가리킨다.
   `core/v1`에 `failed_because`, `assumed`, `unknown_whether` 계열을 추가하는 것은
   minor 버전이므로 캐시를 무효화하지 않는다 (D8).

### R6 — 통제 베이스라인 평가 실행 · **§14 개정**

**D5에 대한 답.** MemDelta의 결론("통제하면 이득이 사라진다")은 oxibrain에 대한 반증 가능성이고,
그걸 회피하는 것보다 정면으로 측정하는 게 낫다.

**할 것 — 같은 코퍼스에 대해 3개 구성:**

| 구성 | 내용 | 역할 |
|---|---|---|
| **(a) full-context** | 전체를 컨텍스트에 넣음 | 상한 |
| **(b) chunk-only** | BM25 + dense 청크 + RRF, **KG 없음** | **통제 베이스라인** |
| **(c) oxibrain full** | 전체 스택 | 실험군 |

**보고 단위는 절대 점수가 아니라 (c) − (b) 델타이고, knowledge update / temporal reasoning
카테고리에서 특히 그렇다.** 이 두 카테고리는 assertion log와 bi-temporal fold가 존재하는
바로 그 이유다. 여기서 델타가 안 나오면 아키텍처가 값을 못 하고 있는 것이고,
로드맵을 바꿔야 한다.

**부가 조치:**

- 토큰/쿼리를 항상 병기 (§14.1이 이미 규정 — 잘 되어 있음)
- `fabricated_entity_rate` 하드코딩 `0.0` 제거 (G11) — 구조적 보장이면 **검증기가 실제로
  걸러낸 건수를 세서** 0을 증명해야지, 상수를 반환하는 건 측정이 아니다
- LongMemEval-V2는 v1 목표 아님. **oxios 통합 후의 목표로 기록만.**

### R7 — 그래프 베팅 헤지

**D6에 대한 답.** 업계가 그래프를 랭킹 신호로 내리는 동안 oxibrain은 질의 구조로 유지한다.
이 베팅의 근거는 확실하다 — 랭킹 신호는 "왜 믿는가/언제부터 참인가/누가 주장했나"에 답할 수 없고,
그게 oxibrain의 전체 가치 제안이다.

**다만 헤지:** 그래프가 **질의 구조로 실패하더라도 랭킹 신호로는 계속 값을 하도록** 만든다.
이미 부분적으로 그렇다 (RRF의 graph 모드, entity salience). 명시적으로 해야 할 것:

- 그래프 근접성을 **하이브리드 융합의 상시 신호**로 (현재는 별도 모드일 때만)
- 공동 등장(co-occurrence) 기반 salience를 §16.2가 계획한 대로 실제 신호로 연결

이렇게 하면 R6의 델타가 실망스럽게 나와도 **총손실이 아니라 부분 회수**가 된다.

**D12(SQLite, 그래프 DB 없음)는 유지.** 이번 레퍼런스 중 그래프 DB를 지지하는 것은 없다 —
autoflow는 TiDB, morphik은 Postgres, Stash는 Postgres+pgvector인데 **전부 서버 형태이기 때문**이다.
반대로 supermemory가 로컬 단일 바이너리를 출시했고 tolaria가 Tauri+Rust 데스크톱인 것은
oxibrain의 제약이 시장에서 검증되고 있다는 신호다.

### R8 — 명시적 기각 목록

방향을 정할 때 **하지 않기로 한 것을 적어두는 것**이 정하는 것만큼 중요하다.

| 기각 | 출처 | 이유 |
|---|---|---|
| 멀티모달 / OCR / ColPali | morphik | §2 non-goal. GPU+Python 요구, 단일 Rust 바이너리와 양립 불가. connector 사전 전사로 해소 |
| LLM에게 삭제권 | mem0, supermemory "automatic forgetting" | **D15 유지.** 지운 것의 기록이 없는 삭제는 감사가능성의 반대 |
| 클라우드 / 멀티테넌트 | supermemory, autoflow, morphik | §2 non-goal. 자체 호스팅 단일 노드가 상한 |
| 위키를 파일로 물질화 | LLM-Wiki, SMFS 문자 그대로 | §1.4 위반 + P1 충돌. **R3의 렌더된 페이지로 대체** |
| DSPy / Python 프롬프트 최적화 | autoflow | Python. "실패를 few-shot으로 되먹임" 아이디어만 흡수 |
| MCP 도구 대량 추가 | Stash (28개) | 2026 방향은 도구를 늘리는 게 아니라 항해 가능하게 만드는 것. **13 → 15가 상한** |

### R9 / R10 — 작은 것들

- **R9 (낮음):** `extraction_failures`를 few-shot 예시로 되먹이는 루프 (2.5)
- **R10 (작음):** incognito 세션 — "이번 대화는 ledger에 쓰지 않는다" (2.10).
  episode를 만들지 않는 것이므로 P1 위반 아님

---

## 6. 제안 아키텍처 델타

기존 4개 zone(Ledger / Cache / Projection / Ops)은 그대로 두고, **표면 층에 하나를 추가**한다.

```
┌──────────────────────────────────────────────────────────────────┐
│ SURFACES                                                         │
│  cli · mcp · rust api · desktop ui                               │
├──────────────────────────────────────────────────────────────────┤
│ ★ VIEWS (신규) — 저장되지 않음, 항상 렌더링됨                      │
│   brief(entity|topic)   프로필    navigate    assemble_context    │
│   └ fold + provenance + uncertainty 의 결정론적 함수               │
├──────────────────────────────────────────────────────────────────┤
│ oxibrain-core (변경 없음)                                         │
│   ingestion │ knowledge │ retrieval │ lifecycle                  │
├──────────────────────────────────────────────────────────────────┤
│ oxibrain-index (확장)                                             │
│   기존: FTS5 · TF-IDF · RRF · adjacency · community              │
│   ★ 신규: contextual chunks · dense vectors · rerank              │
├──────────────────────────────────────────────────────────────────┤
│ PORTS                                                            │
│   LlmPort · EmbeddingPort★(구현) · ClockPort · RerankPort★        │
└──────────────────────────────────────────────────────────────────┘
```

**VIEWS 층의 규칙 — 이게 전체 제안의 핵심이다:**

1. **View는 저장되지 않는다.** 저장되면 그 순간 동기화 문제가 생기고 P1이 깨진다.
2. **View는 결정론적이다** — 캐시된 요약 텍스트를 제외하고. 즉 `brief`를 두 번 부르면 같은 것이 나온다.
3. **모든 View는 불확실성을 동반한다** (R5).
4. **View는 새 데이터를 만들지 않는다.** ledger에 쓰는 것은 여전히 episode뿐이다.

이 층이 있으면 **"에이전트가 무엇을 보는가"가 "브레인이 무엇을 아는가"와 독립적으로 진화할 수
있다.** 지금은 이 둘이 붙어 있어서 `assemble_context`를 고치는 것이 store 코드를 고치는 일이 된다.

---

## 7. 로드맵 제안 — M7 / M8 / M9

### M7 — "측정 가능하게 만들기" (차단 요인 해소)

R1 + R2 + R6. 새 기능보다 **주장한 것을 실제로 동작하게** 만드는 마일스톤.

- `oxibrain-embed-local` + `oxibrain-embed-http`, dense 경로 실연결
- 벡터 결정성 예외를 DESIGN §5.1 / §14.3에 문서화
- `assemble_context`: hybrid 모드, neighborhood 레이어, belief 렌더링 수정, 확장 정책, `dropped`
- 3-구성 통제 평가 실행

**Exit:** LongMemEval에서 (c)−(b) 델타가 knowledge update / temporal reasoning 카테고리에서
**측정되고 기록됨.** 값이 좋든 나쁘든 상관없다 — **숫자가 존재하는 것**이 exit 조건이다.

### M8 — "에이전트 네이티브"

R3 + R4 + Profile.

- `brief` / `navigate` 도구 + `oxibrain page` CLI
- Profile 레이어 (registry `profile_relevant` 플래그 포함)
- contextual chunk (결정론적 prefix) + `RerankPort`
- incognito 세션 (R10)

**Exit:** Claude Desktop이 `brief`로 시작해 `navigate`만으로 3-홉 질문에 답할 수 있음.
그리고 M7 대비 토큰/쿼리가 **줄어듦** (SMFS가 3.0×를 주장한 자리).

### M9 — "정직한 기억"

R5 + R7 + R9.

- 불확실성 블록을 동반하는 consolidation
- registry의 부정/불확실 predicate 계열
- 그래프 근접성을 상시 융합 신호로
- `extraction_failures` → few-shot 되먹임

**Exit:** clawsouls 실험을 우리 코퍼스에서 재현 — 요약 단독 / 원문 단독 / 하이브리드 3구성에서
**하이브리드가 이기고 요약 단독이 무메모리를 넘김.**

---

## 8. 리스크와 반론

| 리스크 | 심각도 | 대응 |
|---|---|---|
| **M7 델타가 실망스럽게 나옴** — KG가 값을 못 함 | **높음** | 이게 정확히 R6의 목적이다. 나쁜 결과가 나오면 R7의 헤지로 부분 회수하고, VIEWS 층(R3)은 KG 강도와 무관하게 값을 한다 |
| 임베딩 도입이 결정성 서사를 훼손 | 중간 | R1의 명시적 예외. 벡터는 순위에만 영향(§9.2), belief에는 무영향 |
| 한국어 성능 | **높음, 미측정** | 공개 벤치마크는 전부 영어. §14.1의 이중언어 골든 코퍼스가 유일한 답인데 **아직 존재하지 않음**. 임베딩 모델 선택 시 다국어 여부가 사실상 결정 요인 |
| 범위 확장 | 중간 | M7은 순전히 부채 상환이고, M8/M9는 각각 독립적으로 멈출 수 있음 |
| 도구/표면 비대화 | 중간 | 13 → 15 상한을 명시적 제약으로 |
| 추출 비용 | 낮음 | 이미 §7.6에서 처리 (배치/야간, 캐시, 샘플링) |

**가장 정직한 반론:** MemDelta가 맞다면, oxibrain이 지은 것의 상당 부분은
(b) 베이스라인이 이미 주는 것을 훨씬 비싸게 사는 일일 수 있다. 이 보고서는 그 가능성을
부정하지 않고 **M7의 exit 조건으로 만든다.** 대신 (b)가 절대 줄 수 없는 것들 —
provenance, `as_of`, 모순 표면화, redaction 클로저, byte-identical 재투영 — 은
벤치마크가 측정하지 않는 값이며, 그게 oxibrain이 제품으로서 존재하는 이유다.
**벤치마크에서 지더라도 그 값들 때문에 사는 사용자가 있는지가 진짜 질문이고,
그건 벤치마크가 아니라 M8 이후에 답이 나온다.**

---

## 9. 결정이 필요한 열린 질문

1. **기본 임베딩 모델.** GGUF 번들(바이너리 비대) / 첫 실행 다운로드(네트워크 필요) /
   HTTP 전용(API 키 필요). **한국어를 고려하면 다국어 모델이 사실상 강제**이고,
   이건 §20의 열린 질문 1에 새 제약을 추가한다.
2. **벡터 테이블의 결정성 예외를 받아들이는가?** (R1) — 받아들이는 쪽을 권고하지만
   P1에 대한 첫 공식 예외이므로 명시적 승인이 필요하다.
3. **v1이 LongMemEval 숫자에 걸리는가?** DESIGN §14.1은 ≥85를 v1 목표로 못박았다.
   MemDelta 이후 **절대 점수보다 통제 델타가 정직한 게이트**라고 보는데, 이건 §14 개정이다.
4. **VIEWS 층을 새 크레이트로 뺄 것인가**(`oxibrain-views`), 아니면 facade 안에 둘 것인가?
   P6(코어는 표면을 모른다)을 지키려면 별도 크레이트가 깨끗하지만 크레이트가 11개가 된다.
5. **이 보고서의 결론 중 어디까지를 DESIGN.md v1.1로 승격하는가?**
   최소한 R5(§10 수정)와 R1의 결정성 예외(§5.1/§14.3)는 코드보다 문서가 먼저 가야 한다.

---

## 부록 — 레퍼런스 목록

**코드베이스** (전부 클론해서 확인)
- [pingcap/autoflow](https://github.com/pingcap/autoflow) — GraphRAG + TiDB + DSPy
- [morphik-org/morphik-core](https://github.com/morphik-org/morphik-core) — ColPali 멀티모달 RAG
- [microsoft/Memora](https://github.com/microsoft/Memora) — harmonic memory representation
- [supermemoryai/supermemory](https://github.com/supermemoryai/supermemory) — 메모리 엔진 + SMFS + profiles
- [refactoringhq/tolaria](https://github.com/refactoringhq/tolaria) — Tauri 2 files-first PKM
- [Stash](https://alash3al.github.io/stash) — episode → structured knowledge, MCP

**논문·글**
- Karpathy, [LLM Wiki gist](https://gist.github.com/karpathy/442a6bf555914893e9891c11519de94f)
- [*Retrieval as Reasoning: Self-Evolving Agent-Native Retrieval via LLM-Wiki*](https://arxiv.org/abs/2605.25480) (arXiv:2605.25480)
- [*MemDelta: Controlled Baselines and Hidden Confounds in Agent Memory Evaluation*](https://arxiv.org/pdf/2606.29914) (arXiv:2606.29914)
- [*LongMemEval-V2*](https://arxiv.org/html/2605.12493v1) (arXiv:2605.12493)
- [Anthropic — Contextual Retrieval](https://www.anthropic.com/news/contextual-retrieval)
- [Anthropic — Claude Memory](https://www.anthropic.com/news/memory)
- [Shlok Khemani — Claude의 메모리 아키텍처](https://www.shloked.com/writing/claude-memory)
- [Meta REFRAG 해설](https://paddedinputs.substack.com/p/meta-superintelligences-surprising)
- [clawsouls — 경험적 메모리 실험](https://blog.clawsouls.ai/ko/posts/experiential-memory-experiment/)
- [mem0 — State of AI Agent Memory 2026](https://mem0.ai/blog/state-of-ai-agent-memory-2026)
- [PyTorch KR — RAG 기술 서베이 1/2](https://discuss.pytorch.kr/t/rag-1-2/3135)
- [Zep: A Temporal Knowledge Graph Architecture for Agent Memory](https://arxiv.org/abs/2501.13956) (기존 §21 레퍼런스, 재확인)
