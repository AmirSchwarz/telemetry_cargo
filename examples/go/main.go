package main

import (
	"context"
	"fmt"
	"telemetry.local/sdk"
)

func main() {
	sdk, err := telemetry.New(telemetry.Config{AppID: "example.go", AppVersion: "1.0"})
	if err != nil {
		panic(err)
	}
	defer func() {
		if err := sdk.Close(context.Background()); err != nil {
			panic(err)
		}
	}()
	// Calls are gated while consent is disabled.
	if err := sdk.Action("search.submit", telemetry.Success); err != nil {
		panic(err)
	}
	if err := sdk.CaptureError("io.error", "synthetic private message"); err != nil {
		panic(err)
	}
	capabilities, err := sdk.Capabilities()
	if err != nil {
		panic(err)
	}
	fmt.Println(string(capabilities))
}
