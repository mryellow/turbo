package core

import (
	"fmt"
	"regexp"
	"testing"

	testifyAssert "github.com/stretchr/testify/assert"
	"github.com/vercel/turbo/cli/internal/fs"
	"github.com/vercel/turbo/cli/internal/graph"
	"github.com/vercel/turbo/cli/internal/util"

	"github.com/pyr-sh/dag"
)

var WorkspaceGraphDefinition = map[string][]string{
	"workspace-a": {"workspace-c"}, // a depends on c
	"workspace-b": {"workspace-c"}, // b depends on c
	"workspace-c": {},
}

func TestPrepare_PersistentDependencies_Topological(t *testing.T) {
	completeGraph, workspaces := _buildCompleteGraph(WorkspaceGraphDefinition)

	engine := NewEngine(&completeGraph.TopologicalGraph)

	// "dev": dependsOn: ["^dev"] (where dev is persistent)
	engine.AddTask(&Task{
		Name:       "dev",
		TopoDeps:   util.SetFromStrings([]string{"dev"}),
		Deps:       make(util.Set), // empty, no non-caret task deps.
		Persistent: true,
	})

	opts := &EngineBuildingOptions{
		Packages:  workspaces,
		TaskNames: []string{"dev"},
	}
	if err := engine.Prepare(opts); err != nil {
		t.Fatalf("Failed to prepare engine: %v", err)
	}

	// do the validation
	actualErr := engine.ValidatePersistentDependencies(completeGraph)

	// Use a regex here, because depending on the order the graph is walked,
	// either workpsce-a or workspace-b could throw the error first.
	expected := regexp.MustCompile("\"workspace-c#dev\" is a persistent task, \"workspace-[a|b]#dev\" cannot depend on it")
	testifyAssert.Regexp(t, expected, actualErr)
}

func TestPrepare_PersistentDependencies_SameWorkspace(t *testing.T) {
	completeGraph, workspaces := _buildCompleteGraph(WorkspaceGraphDefinition)
	engine := NewEngine(&completeGraph.TopologicalGraph)

	// "build": dependsOn: ["dev"] (where build is not, but "dev" is persistent)
	engine.AddTask(&Task{
		Name:       "build",
		TopoDeps:   make(util.Set), // empty
		Deps:       util.SetFromStrings([]string{"dev"}),
		Persistent: false,
	})

	engine.AddTask(&Task{
		Name:       "dev",
		TopoDeps:   make(util.Set),
		Deps:       make(util.Set),
		Persistent: true,
	})

	opts := &EngineBuildingOptions{
		Packages:  workspaces,
		TaskNames: []string{"build"},
	}

	if err := engine.Prepare(opts); err != nil {
		t.Fatalf("Failed to prepare engine: %v", err)
	}

	// do the validation
	actualErr := engine.ValidatePersistentDependencies(completeGraph)

	// Note: this regex is not perfect, becase it doesn't validate that the a|b|c is the same in both positions,
	// but that's ok. It is unlikely that the error message will be wrong here. (And even if it is,
	// the feature that is being tested would still be working)
	expected := regexp.MustCompile("\"workspace-[a|b|c]#dev\" is a persistent task, \"workspace-[a|b|c]#build\" cannot depend on it")
	testifyAssert.Regexp(t, expected, actualErr)
}

func TestPrepare_PersistentDependencies_WorkspaceSpecific(t *testing.T) {
	completeGraph, workspaces := _buildCompleteGraph(WorkspaceGraphDefinition)
	engine := NewEngine(&completeGraph.TopologicalGraph)

	// "build": dependsOn: ["workspace-b#dev"]
	engine.AddTask(&Task{
		Name:       "build",
		TopoDeps:   make(util.Set), // empty
		Deps:       util.SetFromStrings([]string{"workspace-b#dev"}),
		Persistent: false,
	})

	// workspace-b#dev is persistent, and has no dependencies
	engine.AddTask(&Task{
		Name:       "workspace-b#dev",
		TopoDeps:   make(util.Set), // empty
		Deps:       make(util.Set), // empty
		Persistent: true,
	})

	opts := &EngineBuildingOptions{
		Packages:  workspaces,
		TaskNames: []string{"build"},
	}

	if err := engine.Prepare(opts); err != nil {
		t.Fatalf("Failed to prepare engine: %v", err)
	}

	// do the validation
	actualErr := engine.ValidatePersistentDependencies(completeGraph)

	// Depending on the order the graph is walked in, workspace a, b, or c, could throw the error first
	// but the persistent task is consistently workspace-b.
	expected := regexp.MustCompile("\"workspace-b#dev\" is a persistent task, \"workspace-[a|b|c]#build\" cannot depend on it")
	testifyAssert.Regexp(t, expected, actualErr, "")
}

func TestPrepare_PersistentDependencies_CrossWorkspace(t *testing.T) {
	completeGraph, workspaces := _buildCompleteGraph(WorkspaceGraphDefinition)
	engine := NewEngine(&completeGraph.TopologicalGraph)

	// workspace-a#dev specifically dependsOn workspace-b#dev
	// Note: AddDep() is necessary in addition to AddTask() to set up this dependency
	if err := engine.AddDep("workspace-b#dev", "workspace-a#dev"); err != nil {
		t.Fatalf("Something went wrong in test construction: %s", err)
	}

	engine.AddTask(&Task{
		Name:       "workspace-a#dev",
		TopoDeps:   make(util.Set), // empty
		Deps:       util.SetFromStrings([]string{"workspace-b#dev"}),
		Persistent: true,
	})

	// workspace-b#dev dependsOn nothing else
	engine.AddTask(&Task{
		Name:       "workspace-b#dev",
		TopoDeps:   make(util.Set), // empty
		Deps:       make(util.Set), // empty
		Persistent: true,
	})

	opts := &EngineBuildingOptions{
		Packages:  workspaces,
		TaskNames: []string{"dev"},
	}
	if err := engine.Prepare(opts); err != nil {
		t.Fatalf("Failed to prepare engine: %v", err)
	}

	// do the validation
	actualErr := engine.ValidatePersistentDependencies(completeGraph)
	// Depending on the order the graph is walked in, workspace a, b, or c, could throw the error first
	// but the persistent task is consistently workspace-b.
	expected := regexp.MustCompile("\"workspace-b#dev\" is a persistent task, \"workspace-a#dev\" cannot depend on it")
	testifyAssert.Regexp(t, expected, actualErr, "")
}

func TestPrepare_PersistentDependencies_Unimplemented(t *testing.T) {
	completeGraph, workspaces := _buildCompleteGraph(WorkspaceGraphDefinition)
	engine := NewEngine(&completeGraph.TopologicalGraph)

	// Remove "dev" script from workspace-c. workspace-a|b will still implement,
	// but since no topological dependencies implement, this test can ensure there is no error
	delete(completeGraph.PackageInfos["workspace-c"].Scripts, "dev")

	// "dev": dependsOn: ["^dev"] (dev is persistent, but workspace-c does not implement dev)
	engine.AddTask(&Task{
		Name:       "dev",
		TopoDeps:   util.SetFromStrings([]string{"dev"}),
		Deps:       make(util.Set), // empty, no non-caret task deps.
		Persistent: true,
	})

	opts := &EngineBuildingOptions{
		Packages:  workspaces,
		TaskNames: []string{"dev"},
	}
	if err := engine.Prepare(opts); err != nil {
		t.Fatalf("Failed to prepare engine: %v", err)
	}

	// do the validation
	actualErr := engine.ValidatePersistentDependencies(completeGraph)

	testifyAssert.Nil(t, actualErr)
}

// helper function for some of the tests to set up workspace
func _buildCompleteGraph(workspaceEasyDefinition map[string][]string) (*graph.CompleteGraph, []string) {
	var workspaceGraph dag.AcyclicGraph
	var workspaces []string

	// Turn the easy definition above into a dag.AcyclicGraph
	// Also collect just the keys of the easyDefinition
	for workspace, dependsOn := range workspaceEasyDefinition {
		workspaces = append(workspaces, workspace)
		workspaceGraph.Add(workspace)
		for _, dependsOnWorkspace := range dependsOn {
			workspaceGraph.Connect(dag.BasicEdge(workspace, dependsOnWorkspace))
		}
	}

	// build Workspace Infos
	workspaceInfos := make(graph.WorkspaceInfos)
	for _, workspace := range workspaces {
		workspaceInfos[workspace] = &fs.PackageJSON{
			Name: workspace,
			Scripts: map[string]string{
				"build": fmt.Sprintf("echo \"%s build\"", workspace),
				"dev":   fmt.Sprintf("echo \"%s dev\"", workspace),
			},
		}
	}

	// build completeGraph struct
	completeGraph := &graph.CompleteGraph{
		TopologicalGraph: workspaceGraph,
		PackageInfos:     workspaceInfos,
	}

	return completeGraph, workspaces
}