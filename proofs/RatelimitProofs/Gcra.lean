/-
Copyright (c) 2026 WyattAu. All rights reserved.
Formal verification: GCRA rate limiter (integer-microsecond core in
`src/gcra.rs`; ported verbatim from crawlkit's ProofGcra.lean,
YP-CRAWL-TOKENBUCKET-001 v2.0.0).

Machine-checked with Lean 4.35.0 + Mathlib v4.35.0.
Zero `sorry`, zero new axioms.
-/

import Mathlib.Tactic

/-!
# GCRA Rate Limiter

Formalization of the Generic Cell Rate Algorithm in virtual-scheduling form
(ITU-T I.371 / Q.2963) as specified in YP-CRAWL-TOKENBUCKET-001 v2.0.0.

Time is modeled over `ℚ`, which embeds the implementation's integer microseconds
exactly; every theorem statement holds verbatim under that embedding.

Parameters: `T > 0` (emission interval), `τ ≥ 0` (burst tolerance),
`b ≥ 1` (burst capacity) with `(b-1)·T ≤ τ < b·T` — i.e. `b = 1 + ⌊τ/T⌋`.

## Verified properties

* THM-RATE-001 `run_monotone`       — TAT never decreases (clock-skew robustness)
* THM-RATE-002 `bounded_debt`       — after an admission at `u`, `TAT ≤ u + τ + T`
* THM-RATE-003 `spacing_k`          — k-th admission `t_k ≥ TAT₀ + (k-1)·T − τ`
* THM-RATE-004 `pair_spacing`       — `t_j − t_i ≥ (j−i)·T − τ`
* THM-RATE-005 `burst_same_instant` — at most `b` admissions at one instant (tight)
* THM-RATE-006 `window_bound`       — `k` admissions in a half-open window of length `d`
  imply `(k−1)·T − τ < d`; corollary `int_window_bound`: `(k−b)·T < d`
* THM-RATE-007 `conforms_mono`      — a later arrival never breaks conformance
-/

namespace Ratelimit.Gcra

/-- Limiter parameters. `b` is the burst capacity: `(b-1)·T ≤ τ < b·T`. -/
structure Params where
  /-- Emission interval (µs per request). -/
  T : ℚ
  /-- Burst tolerance (µs). -/
  tau : ℚ
  /-- Burst capacity (requests); must be positive. -/
  b : ℕ
  T_pos : 0 < T
  tau_nonneg : 0 ≤ tau
  b_pos : 0 < b
  b_lo : ((b - 1 : ℕ) : ℚ) * T ≤ tau
  b_hi : tau < (b : ℚ) * T

/-- A request arriving at time `t` with limiter state `tat` conforms iff `t ≥ tat − τ`. -/
def Conforms (p : Params) (tat t : ℚ) : Prop := t ≥ tat - p.tau

/-- One GCRA transition (virtual scheduling): `TAT ← max(TAT, t) + T`. -/
def gcraStep (p : Params) (tat t : ℚ) : ℚ := max tat t + p.T

/-- Final TAT after processing a trace of (conforming) admission times. -/
def run (p : Params) (tat : ℚ) : List ℚ → ℚ
  | [] => tat
  | t :: ts => run p (gcraStep p tat t) ts

/-- Well-formed trace: every arrival conforms at the state in which it is processed. -/
inductive WF (p : Params) : ℚ → List ℚ → Prop
  | wf_nil {tat : ℚ} : WF p tat []
  | wf_cons {tat t : ℚ} {ts : List ℚ} :
      Conforms p tat t → WF p (gcraStep p tat t) ts → WF p tat (t :: ts)

/-! ### List helpers -/

theorem length_cons_sub {α : Type*} (a : α) (l : List α) :
    (a :: l).length - 1 = l.length := by simp

/-- `l[m]? = some u` implies `m < l.length`. -/
theorem lt_of_get? : ∀ (l : List ℚ) (m : ℕ) (u : ℚ), l[m]? = some u → m < l.length
  | [], m, u, h => by simp at h
  | a :: l', 0, u, h => by simpa using h
  | a :: l', (n + 1), u, h => by
      simp only [List.getElem?_cons_succ] at h
      have h2 := lt_of_get? l' n u h
      rw [List.length_cons]
      omega

/-! ### Basic step lemmas -/

theorem gcraStep_ge_tat (p : Params) (tat t : ℚ) : tat ≤ gcraStep p tat t := by
  unfold gcraStep
  linarith [le_max_left tat t, p.T_pos]

theorem gcraStep_ge_t (p : Params) (tat t : ℚ) : t + p.T ≤ gcraStep p tat t := by
  unfold gcraStep
  linarith [le_max_right tat t, p.T_pos]

theorem gcraStep_le_of_conforms {p : Params} {tat t : ℚ} (h : Conforms p tat t) :
    gcraStep p tat t ≤ t + p.tau + p.T := by
  have hτ := p.tau_nonneg
  unfold Conforms at h
  unfold gcraStep
  rcases le_total tat t with h' | h'
  · rw [max_eq_right h']; all_goals linarith
  · rw [max_eq_left h']; all_goals linarith

theorem gcraStep_mono_t (p : Params) (tat : ℚ) {t t' : ℚ} (h : t ≤ t') :
    gcraStep p tat t ≤ gcraStep p tat t' := by
  have hT := p.T_pos
  unfold gcraStep
  have h1 : max tat t ≤ max tat t' := max_le_max le_rfl h
  linarith

/-! ### THM-RATE-007: decision monotonicity (no-regret, per request) -/

theorem conforms_mono (p : Params) (tat : ℚ) {t t' : ℚ} (h : t ≤ t') (hc : Conforms p tat t) :
    Conforms p tat t' := by
  unfold Conforms at *
  linarith

/-! ### THM-RATE-001: TAT monotonicity (clock-skew robustness) -/

theorem run_monotone (p : Params) (tat : ℚ) : ∀ ts : List ℚ, tat ≤ run p tat ts
  | [] => le_refl tat
  | t :: ts =>
    le_trans (gcraStep_ge_tat p tat t)
      (run_monotone p (gcraStep p tat t) ts)

/-! ### Structural lemmas -/

theorem run_append (p : Params) (tat : ℚ) :
    ∀ l l' : List ℚ, run p tat (l ++ l') = run p (run p tat l) l'
  | [], _ => rfl
  | t :: l, l' => run_append p (gcraStep p tat t) l l'

theorem run_ge_linear (p : Params) (tat : ℚ) :
    ∀ ts : List ℚ, tat + (ts.length : ℚ) * p.T ≤ run p tat ts
  | [] => by simp [run]
  | t :: ts => by
      have ih := run_ge_linear p (gcraStep p tat t) ts
      have h2 : tat + p.T ≤ gcraStep p tat t := by
        unfold gcraStep
        linarith [le_max_left tat t]
      have hlen : (t :: ts).length = ts.length + 1 := rfl
      calc tat + ((t :: ts).length : ℚ) * p.T
          = tat + (((ts.length : ℚ) + 1) * p.T) := by rw [hlen]; push_cast; ring
        _ = (tat + p.T) + (ts.length : ℚ) * p.T := by ring
        _ ≤ gcraStep p tat t + (ts.length : ℚ) * p.T := by linarith
        _ ≤ run p (gcraStep p tat t) ts := ih

/-- Splitting a well-formed trace: the suffix is well-formed at the prefix's final state. -/
theorem wf_split (p : Params) (tat : ℚ) : ∀ {ts : List ℚ}, WF p tat ts →
    ∀ m : ℕ, m ≤ ts.length → WF p (run p tat (ts.take m)) (ts.drop m)
  | [], .wf_nil, m, hm => by
      have : m = 0 := by
        cases m with
        | zero => rfl
        | succ n => exact absurd hm (by simp)
      subst this
      exact WF.wf_nil
  | t :: ts, .wf_cons hconf hrest, 0, _ => by
      simpa [run] using WF.wf_cons hconf hrest
  | t :: ts, .wf_cons hconf hrest, (m + 1), hm => by
      rw [List.length_cons] at hm
      have hmlt : m ≤ ts.length := by omega
      have ih := wf_split p (gcraStep p tat t) hrest m hmlt
      have hrun : run p tat ((t :: ts).take (m + 1))
                  = run p (gcraStep p tat t) (ts.take m) := by
        simp [run]
      have hdrop : (t :: ts).drop (m + 1) = ts.drop m :=
        List.drop_cons (a := t) (l := ts) (i := m + 1) (by omega)
      rw [hrun, hdrop]
      exact ih

/-- Prefix of a well-formed trace is well-formed. -/
theorem wf_take (p : Params) (tat : ℚ) : ∀ {ts : List ℚ}, WF p tat ts →
    ∀ m : ℕ, m ≤ ts.length → WF p tat (ts.take m)
  | [], .wf_nil, m, hm => by
      have : m = 0 := by
        cases m with
        | zero => rfl
        | succ n => exact absurd hm (by simp)
      subst this
      exact WF.wf_nil
  | t :: ts, .wf_cons hconf _, 0, _ => WF.wf_nil
  | t :: ts, .wf_cons hconf hrest, (m + 1), hm => by
      rw [List.length_cons] at hm
      have hmlt : m ≤ ts.length := by omega
      have htake : (t :: ts).take (m + 1) = t :: ts.take m :=
        List.take_cons (a := t) (l := ts) (i := m + 1) (by omega)
      rw [htake]
      exact WF.wf_cons hconf (wf_take p (gcraStep p tat t) hrest m hmlt)

/-- `l.take (n+1) = l.take n ++ [l[n]]` in `get?` form. -/
theorem take_succ_get? : ∀ (l : List ℚ) (n : ℕ) (u : ℚ), l[n]? = some u →
    l.take (n + 1) = l.take n ++ [u]
  | l, 0, u, h => by
      cases l with
      | nil => simp at h
      | cons a l' =>
          simp only [List.getElem?_cons_zero] at h
          simp at h
          subst h
          simp
  | l, (n + 1), u, h => by
      cases l with
      | nil => simp at h
      | cons a l' =>
          simp only [List.getElem?_cons_succ] at h
          rw [List.take_cons (show 0 < n + 1 + 1 by omega)]
          show a :: l'.take (n + 1) = a :: l'.take n ++ [u]
          rw [take_succ_get? l' n u h]
          rfl

/-- `l[k]? = some u` puts `u` at the head of `l.drop k`. -/
theorem get?_drop_head (l : List ℚ) : ∀ (k : ℕ) (u : ℚ), k < l.length →
    l[k]? = some u → (l.drop k).head? = some u
  | 0, u, _, h => by
      cases l with
      | nil => simp at h
      | cons a l' =>
          simp only [List.getElem?_cons_zero] at h
          simp at h
          subst h
          simp
  | k + 1, u, hk, h => by
      cases l with
      | nil => simp at hk
      | cons a l' =>
          simp only [List.getElem?_cons_succ] at h
          rw [List.drop_cons (show 0 < k + 1 by omega)]
          show (l'.drop k).head? = some u
          rw [List.length_cons] at hk
          have hk' : k < l'.length := by omega
          exact get?_drop_head l' k u hk' h

/-- `l[i]? = some u` implies `u ∈ l` when `i < l.length`. -/
theorem get?_mem (l : List ℚ) : ∀ (i : ℕ) (u : ℚ), l[i]? = some u → i < l.length → u ∈ l
  | 0, u, h, _ => by
      cases l with
      | nil => simp at h
      | cons a l' =>
          simp only [List.getElem?_cons_zero] at h
          simp at h
          subst h
          exact List.mem_cons_self ..
  | i + 1, u, h, hlt => by
      cases l with
      | nil => simp at h
      | cons a l' =>
          simp only [List.getElem?_cons_succ] at h
          rw [List.length_cons] at hlt
          exact List.mem_cons_of_mem _ (get?_mem l' i u h (by omega))

theorem get?_exists_last : ∀ (l : List ℚ), l ≠ [] → ∃ u, l[l.length - 1]? = some u
  | [], hne => absurd rfl hne
  | [a], _ => ⟨a, by simp⟩
  | a :: b :: l'', _ => by
      have h1 : (a :: b :: l'').length - 1 = l''.length + 1 := by
        rw [length_cons_sub, List.length_cons]
      rw [h1, List.getElem?_cons_succ]
      obtain ⟨u, hu⟩ := get?_exists_last (b :: l'') (by simp)
      refine ⟨u, ?_⟩
      have h2 : (b :: l'').length - 1 = l''.length := length_cons_sub ..
      rwa [← h2]

/-- Conformance of element `j` at the state after `j` prior admissions. -/
theorem get_conforms_at_prefix (p : Params) (tat : ℚ) : ∀ (ts : List ℚ), WF p tat ts →
    ∀ (j : ℕ) (u : ℚ), j < ts.length → ts[j]? = some u →
    Conforms p (run p tat (ts.take j)) u
  | ts, h, j, u, hj, hget => by
      have hsplit := wf_split p tat h j (by omega)
      have hdropne : ts.drop j ≠ [] := by
        intro hnil
        have h1 : (ts.drop j).length = 0 := by rw [hnil]; simp
        rw [List.length_drop] at h1
        omega
      cases hdj : ts.drop j with
      | nil => exact absurd hdj hdropne
      | cons v rest =>
          rw [hdj] at hsplit
          cases hsplit with
          | wf_cons hconf _ =>
              have hhead : (ts.drop j).head? = some u := get?_drop_head ts j u hj hget
              rw [hdj] at hhead
              simp at hhead
              subst hhead
              exact hconf

/-! ### THM-RATE-003: k-th admission spacing -/

theorem spacing_k (p : Params) (tat : ℚ) : ∀ (ts : List ℚ), WF p tat ts →
    ∀ (k : ℕ) (u : ℚ), k < ts.length → ts[k]? = some u →
    u ≥ tat + (k : ℚ) * p.T - p.tau
  | ts, h, k, u, hk, hget => by
      have hconf := get_conforms_at_prefix p tat ts h k u hk hget
      have hlin := run_ge_linear p tat (ts.take k)
      have htake : (ts.take k).length = k := by
        rw [List.length_take]
        omega
      rw [htake] at hlin
      unfold Conforms at hconf
      linarith

/-! ### THM-RATE-004: pair spacing -/

theorem pair_spacing (p : Params) (tat : ℚ) : ∀ (ts : List ℚ), WF p tat ts →
    ∀ i j : ℕ, ∀ tᵢ tⱼ : ℚ, i < j → j < ts.length →
    ts[i]? = some tᵢ → ts[j]? = some tⱼ →
    tⱼ ≥ tᵢ + ((j - i : ℕ) : ℚ) * p.T - p.tau := by
  intro ts h i j tᵢ tⱼ hij hj hi hjget
  have hj1 : i + 1 ≤ j := by omega
  -- The state after i+1 admissions is at least tᵢ + T.
  have hS : run p tat (ts.take (i + 1)) ≥ tᵢ + p.T := by
    have h1 : ts.take (i + 1) = ts.take i ++ [tᵢ] := take_succ_get? ts i tᵢ hi
    rw [h1, run_append]
    have h2 : run p (run p tat (ts.take i)) [tᵢ] = gcraStep p (run p tat (ts.take i)) tᵢ := by
      simp [run]
    rw [h2]
    exact gcraStep_ge_t p _ tᵢ
  -- The state before admission j is at least S + (j - i - 1)·T.
  have hmin1 : ts.take (i + 1) = (ts.take j).take (i + 1) := by
    rw [List.take_take]
    congr 1
    omega
  have hpre : ts.take j = ts.take (i + 1) ++ (ts.take j).drop (i + 1) := by
    rw [hmin1]
    exact (List.take_append_drop (i + 1) (ts.take j)).symm
  have hlen : ((ts.take j).drop (i + 1)).length = j - (i + 1) := by
    rw [List.length_drop, List.length_take]
    omega
  have hSj : run p tat (ts.take j)
             ≥ run p tat (ts.take (i + 1)) + ((j - (i + 1) : ℕ) : ℚ) * p.T := by
    rw [hpre, run_append]
    have hlin := run_ge_linear p (run p tat (ts.take (i + 1))) ((ts.take j).drop (i + 1))
    rw [hlen] at hlin
    exact hlin
  -- Conformance of admission j.
  have hconf := get_conforms_at_prefix p tat ts h j tⱼ hj hjget
  unfold Conforms at hconf
  have hj2 : (j - i : ℕ) = (j - (i + 1) : ℕ) + 1 := by omega
  rw [hj2]
  push_cast
  ring_nf at hS hSj ⊢
  linarith

/-! ### THM-RATE-002: Bounded debt (recovery bound) -/

theorem bounded_debt (p : Params) (tat : ℚ) : ∀ (ts : List ℚ), WF p tat ts →
    ∀ u : ℚ, ts[ts.length - 1]? = some u → run p tat ts ≤ u + p.tau + p.T
  | [], .wf_nil, u, h => by simp at h
  | ts, h, u, hlast => by
      have hne : ts ≠ [] := by
        cases ts with
        | nil => simp at hlast
        | cons a l => simp
      have hpos : 0 < ts.length := List.length_pos_of_ne_nil hne
      have htake : ts.take (ts.length - 1) ++ [u] = ts := by
        have h2 := take_succ_get? ts (ts.length - 1) u hlast
        rw [show ts.length - 1 + 1 = ts.length by omega, List.take_length] at h2
        exact h2.symm
      have hconf := get_conforms_at_prefix p tat ts h (ts.length - 1) u (by omega) hlast
      calc run p tat ts
          = run p tat (ts.take (ts.length - 1) ++ [u]) := by rw [htake]
        _ = run p (run p tat (ts.take (ts.length - 1))) [u] := run_append _ _ _ _
        _ = gcraStep p (run p tat (ts.take (ts.length - 1))) u := rfl
        _ ≤ u + p.tau + p.T := gcraStep_le_of_conforms hconf

/-! ### THM-RATE-005: tight same-instant burst bound -/

theorem burst_same_instant (p : Params) (tat : ℚ) (t₀ : ℚ) :
    ∀ ts : List ℚ, WF p tat ts → (∀ x : ℚ, x ∈ ts → x = t₀) → ts.length ≤ p.b
  | [], _, _ => by simp
  | [a], _, _ => by
      have hb : 1 ≤ p.b := Nat.succ_le_of_lt p.b_pos
      simp only [List.length_singleton]
      exact hb
  | a :: v :: rest', h, hall => by
      have ha : a = t₀ := hall a (List.mem_cons_self ..)
      have hall' : ∀ x : ℚ, x ∈ a :: v :: rest' → x = a := by
        intro x hx
        have hx0 := hall x hx
        rw [hx0, ha]
      have hne : (a :: v :: rest') ≠ [] := by simp
      have hfirst : (a :: v :: rest')[0]? = some a := by simp
      obtain ⟨u, hu⟩ := get?_exists_last (a :: v :: rest') hne
      have hlen : (a :: v :: rest').length = rest'.length + 2 := by
        rw [List.length_cons, List.length_cons]
      have hulen : (a :: v :: rest').length - 1 < (a :: v :: rest').length := by omega
      have humem : u ∈ a :: v :: rest' := get?_mem _ _ u hu hulen
      have hu' : u = a := hall' u humem
      have hj : 0 < (a :: v :: rest').length - 1 := by omega
      have hj2 : (a :: v :: rest').length - 1 < (a :: v :: rest').length := by omega
      have hps := pair_spacing p tat (a :: v :: rest') h 0
        ((a :: v :: rest').length - 1) a u hj hj2 hfirst hu
      simp only [Nat.sub_zero] at hps
      rw [hu'] at hps
      -- (len−1)·T ≤ τ < b·T  →  len−1 < b  →  len ≤ b
      have hX : (((a :: v :: rest').length - 1 : ℕ) : ℚ) * p.T ≤ p.tau := by linarith
      have h1 : (((a :: v :: rest').length - 1 : ℕ) : ℚ) < (p.b : ℚ) := by
        nlinarith [hX, p.b_hi, p.T_pos]
      have h2 : (a :: v :: rest').length - 1 < p.b := by exact_mod_cast h1
      omega

/-! ### THM-RATE-006: window bound (Cruz arrival curve) -/

/-- Any `k ≥ 1` admissions inside a half-open window of length `d` satisfy
`(k−1)·T − τ < d`. -/
theorem window_bound (p : Params) (tat : ℚ) : ∀ (ts : List ℚ), WF p tat ts → ts ≠ [] →
    ∀ t d : ℚ, (∀ x : ℚ, x ∈ ts → t ≤ x ∧ x < t + d) →
    (((ts.length - 1 : ℕ) : ℚ) * p.T - p.tau) < d
  | [], _, hne, _, _, _ => absurd rfl hne
  | [a], _, _, t, d, hmem => by
      have hx := hmem a (List.mem_cons_self ..)
      have hτ := p.tau_nonneg
      have h1 : (([a].length - 1 : ℕ) : ℚ) = 0 := by simp
      rw [h1]
      simp only [zero_mul, zero_sub]
      linarith
  | a :: v :: rest', h, _, t, d, hmem => by
      have hne : (a :: v :: rest') ≠ [] := by simp
      have hfirst : (a :: v :: rest')[0]? = some a := by simp
      obtain ⟨u, hu⟩ := get?_exists_last (a :: v :: rest') hne
      have hlen : (a :: v :: rest').length = rest'.length + 2 := by
        rw [List.length_cons, List.length_cons]
      have hulen : (a :: v :: rest').length - 1 < (a :: v :: rest').length := by omega
      have humem : u ∈ a :: v :: rest' := get?_mem _ _ u hu hulen
      have hj : 0 < (a :: v :: rest').length - 1 := by omega
      have hj2 : (a :: v :: rest').length - 1 < (a :: v :: rest').length := by omega
      have hps := pair_spacing p tat (a :: v :: rest') h 0
        ((a :: v :: rest').length - 1) a u hj hj2 hfirst hu
      simp only [Nat.sub_zero] at hps
      have hx1 := hmem a (List.mem_cons_self ..)
      have hx2 := hmem u humem
      have hτ := p.tau_nonneg
      linarith

/-- Integer-burst corollary: when `τ = (b−1)·T` (the common integer configuration),
`k` admissions in a window of length `d` satisfy `(k−b)·T < d` — Cruz's (σ,ρ) bound. -/
theorem int_window_bound (p : Params) (tat : ℚ) (ts : List ℚ) (h : WF p tat ts)
    (hne : ts ≠ []) (t d : ℚ) (hmem : ∀ x : ℚ, x ∈ ts → t ≤ x ∧ x < t + d)
    (htau : p.tau = ((p.b - 1 : ℕ) : ℚ) * p.T) :
    (((ts.length : ℚ) - (p.b : ℚ)) * p.T) < d := by
  have hpos : 0 < ts.length := List.length_pos_of_ne_nil hne
  have hb1 : (1 : ℕ) ≤ p.b := Nat.succ_le_of_lt p.b_pos
  have hlc : ((ts.length - 1 : ℕ) : ℚ) = (ts.length : ℚ) - 1 := Nat.cast_sub (Nat.succ_le_of_lt hpos)
  have hbc : ((p.b - 1 : ℕ) : ℚ) = (p.b : ℚ) - 1 := Nat.cast_sub hb1
  have hwb := window_bound p tat ts h hne t d hmem
  rw [htau, hlc, hbc] at hwb
  ring_nf at hwb ⊢
  linarith

end Ratelimit.Gcra
