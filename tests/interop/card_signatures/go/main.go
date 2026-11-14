// Command go verifies a PALS Agent Card signature with the official Go SDK,
// a2a-go (a2acrypto), as pinned in go.mod.
//
// Usage, from the repository root (the module is this directory):
//
//	go -C tests/interop/card_signatures/go run . "$PWD/crates/pals-core/tests/vectors/card-jws-v1.json"
//
// The vector is crates/pals-core/tests/vectors/card-jws-v1.json: its
// signed_card is checked as the Go resolver checks a card it fetched
// (agentcard.DefaultCardParser, then a2acrypto.Verifier.Verify over the
// card's bytes for each signature until one verifies), with a
// KeyResolverFunc that trusts only agent_id: the Ed25519 public key whose
// hex is the kid, and only for that kid. A copy with skills[0].description
// changed must fail, and so must the card when another agent is expected.
// Prints "verified" and exits 0, or names the difference and exits 1.
package main

import (
	"context"
	"crypto"
	"crypto/ed25519"
	"encoding/hex"
	"encoding/json"
	"errors"
	"fmt"
	"os"

	"github.com/a2aproject/a2a-go/v2/a2aclient/agentcard"
	"github.com/a2aproject/a2a-go/v2/a2acrypto"
)

// otherAgentID is the id of the agent whose secret key is 32 bytes of 0x02.
const otherAgentID = "8139770ea87d175f56a35466c34c7ecccb8d8a91b4ee37a25df60f5b8fc9b394"

func main() {
	if err := run(); err != nil {
		fmt.Println("FAIL:", err)
		os.Exit(1)
	}
	fmt.Println("verified")
}

func run() error {
	if len(os.Args) != 2 {
		return errors.New("usage: go -C tests/interop/card_signatures/go run . <vector.json>")
	}
	data, err := os.ReadFile(os.Args[1])
	if err != nil {
		return err
	}
	var vector struct {
		AgentID    string          `json:"agent_id"`
		SignedCard json.RawMessage `json:"signed_card"`
	}
	if err := json.Unmarshal(data, &vector); err != nil {
		return fmt.Errorf("vector: %w", err)
	}

	if err := verify(vector.SignedCard, vector.AgentID); err != nil {
		return fmt.Errorf("the card does not verify: %w", err)
	}

	var tampered map[string]any
	if err := json.Unmarshal(vector.SignedCard, &tampered); err != nil {
		return err
	}
	skill := tampered["skills"].([]any)[0].(map[string]any)
	skill["description"] = skill["description"].(string) + " (tampered)"
	tamperedBytes, err := json.Marshal(tampered)
	if err != nil {
		return err
	}
	if verify(tamperedBytes, vector.AgentID) == nil {
		return errors.New("a card with a changed skills[0].description verified")
	}

	if otherAgentID == vector.AgentID {
		return errors.New("the other agent id is the expected one")
	}
	if verify(vector.SignedCard, otherAgentID) == nil {
		return fmt.Errorf("the card verified with %s expected", otherAgentID)
	}
	return nil
}

// verify returns nil when one of the card's signatures verifies with the key
// of expected, an agent id in hex.
func verify(body []byte, expected string) error {
	card, err := agentcard.DefaultCardParser(body)
	if err != nil {
		return fmt.Errorf("parse: %w", err)
	}
	if len(card.Signatures) == 0 {
		return errors.New("no signatures")
	}
	resolver := a2acrypto.KeyResolverFunc(func(_ context.Context, kid string) (crypto.PublicKey, error) {
		if kid != expected {
			return nil, fmt.Errorf("untrusted kid %q", kid)
		}
		key, err := hex.DecodeString(kid)
		if err != nil || len(key) != ed25519.PublicKeySize {
			return nil, fmt.Errorf("kid %q is not an Ed25519 public key in hex", kid)
		}
		return ed25519.PublicKey(key), nil
	})
	verifier := a2acrypto.NewVerifier(a2acrypto.VerifierConfig{KeyResolver: resolver})
	var errs []error
	for _, sig := range card.Signatures {
		err := verifier.Verify(context.Background(), body, &sig)
		if err == nil {
			return nil
		}
		errs = append(errs, err)
	}
	return errors.Join(errs...)
}
