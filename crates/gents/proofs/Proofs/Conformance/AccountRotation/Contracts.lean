import Proofs.Shared.AccountRotation.Cases
import Proofs.Conformance.Functions.Shared

namespace AccountRotation.Contracts

open Lean

deriving instance ToJson for Severity, Limit, Account, Policy

structure SelectInput where
  name : String
  policy : Policy
  now : Nat
  family : Family
  pool : List Account
  current : Option Nat
  cursor : Nat
  deriving ToJson

@[conformance_family]
def selection : Conformance.Family SelectInput (Option Nat) where
  inputs := selectCases.map fun c =>
    { name := c.name, policy := c.policy, now := caseNow, family := c.family, pool := c.pool
      current := c.current, cursor := c.cursor }
  model := fun i => select i.policy i.now i.family i.pool i.current i.cursor

structure Rejection where
  account : Nat
  resetAt : Nat
  deriving ToJson

structure AttemptInput where
  name : String
  policy : Policy
  now : Nat
  family : Family
  pool : List Account
  current : Option Nat
  cursor : Nat
  rejects : List Rejection
  deriving ToJson

@[conformance_family]
def attempts : Conformance.Family AttemptInput (List Nat) where
  inputs := attemptCases.map fun c =>
    { name := c.name, policy := c.policy, now := caseNow, family := c.family, pool := c.pool
      current := c.current, cursor := c.cursor
      rejects := c.rejects.map fun (account, resetAt) => { account, resetAt } }
  model := fun i =>
    attempt i.policy i.now i.family (rejectsOf (i.rejects.map fun r => (r.account, r.resetAt)))
      i.pool.length i.pool i.current i.cursor

structure ObserveInput where
  name : String
  family : Family
  account : Account
  reported : List Limit
  overage : Overage
  deriving ToJson

@[conformance_family]
def observation : Conformance.Family ObserveInput Account where
  inputs := observeCases.map fun c =>
    { name := c.name, family := c.family, account := c.account, reported := c.reported
      overage := c.overage }
  model := fun i => observe i.family i.reported i.overage i.account

end AccountRotation.Contracts
