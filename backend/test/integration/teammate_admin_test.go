//go:build integration
// +build integration

package integration

import (
	"bytes"
	"encoding/json"
	"net/http"
	"net/http/httptest"
	"testing"

	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"

	"hopp-backend/internal/models"
	"hopp-backend/internal/server"
)

// newTeamMember creates a user in the given team.
func newTeamMember(t *testing.T, srv *server.Server, team *models.Team, email string, isAdmin bool) *models.User {
	u := &models.User{
		FirstName: "Test",
		LastName:  email,
		Email:     email,
		Password:  "password123",
		TeamID:    &team.ID,
		IsAdmin:   isAdmin,
	}
	require.NoError(t, srv.DB.Create(u).Error)
	return u
}

func setTeammateAdmin(t *testing.T, srv *server.Server, caller *models.User, targetID string, body any) *httptest.ResponseRecorder {
	b, err := json.Marshal(body)
	require.NoError(t, err)
	req := httptest.NewRequest(http.MethodPut, "/api/auth/teammates/"+targetID+"/admin", bytes.NewReader(b))
	req.Header.Set("Content-Type", "application/json")
	req.Header.Set("Authorization", "Bearer "+getJWTToken(t, srv, caller.Email))
	rec := httptest.NewRecorder()
	srv.Echo.ServeHTTP(rec, req)
	return rec
}

func isAdminInDB(t *testing.T, srv *server.Server, id string) bool {
	var u models.User
	require.NoError(t, srv.DB.Select("id, is_admin").Where("id = ?", id).First(&u).Error)
	return u.IsAdmin
}

func TestSetTeammateAdmin_GrantAndRevoke(t *testing.T) {
	srv, cleanup := setupTestServerFast(t)
	defer cleanup()

	team := createTestTeam(t, srv.DB, "Admins")
	admin := newTeamMember(t, srv, team, "admin@example.com", true)
	member := newTeamMember(t, srv, team, "member@example.com", false)

	rec := setTeammateAdmin(t, srv, admin, member.ID, map[string]any{"is_admin": true})
	require.Equal(t, http.StatusOK, rec.Code, rec.Body.String())
	var resp map[string]any
	require.NoError(t, json.Unmarshal(rec.Body.Bytes(), &resp))
	assert.Equal(t, member.ID, resp["id"])
	assert.Equal(t, true, resp["is_admin"])
	assert.True(t, isAdminInDB(t, srv, member.ID))

	rec = setTeammateAdmin(t, srv, admin, member.ID, map[string]any{"is_admin": false})
	require.Equal(t, http.StatusOK, rec.Code, rec.Body.String())
	assert.False(t, isAdminInDB(t, srv, member.ID))
}

func TestSetTeammateAdmin_NonAdminForbidden(t *testing.T) {
	srv, cleanup := setupTestServerFast(t)
	defer cleanup()

	team := createTestTeam(t, srv.DB, "Admins")
	newTeamMember(t, srv, team, "admin@example.com", true)
	member := newTeamMember(t, srv, team, "member@example.com", false)
	other := newTeamMember(t, srv, team, "other@example.com", false)

	rec := setTeammateAdmin(t, srv, member, other.ID, map[string]any{"is_admin": true})
	assert.Equal(t, http.StatusForbidden, rec.Code)
	assert.False(t, isAdminInDB(t, srv, other.ID))

	// Nor may a non-admin promote themselves.
	rec = setTeammateAdmin(t, srv, member, member.ID, map[string]any{"is_admin": true})
	assert.Equal(t, http.StatusForbidden, rec.Code)
	assert.False(t, isAdminInDB(t, srv, member.ID))
}

func TestSetTeammateAdmin_OtherTeamForbidden_UnknownNotFound(t *testing.T) {
	srv, cleanup := setupTestServerFast(t)
	defer cleanup()

	team1 := createTestTeam(t, srv.DB, "Team One")
	team2 := createTestTeam(t, srv.DB, "Team Two")
	admin := newTeamMember(t, srv, team1, "admin1@example.com", true)
	outsider := newTeamMember(t, srv, team2, "member2@example.com", false)

	rec := setTeammateAdmin(t, srv, admin, outsider.ID, map[string]any{"is_admin": true})
	assert.Equal(t, http.StatusForbidden, rec.Code)
	assert.False(t, isAdminInDB(t, srv, outsider.ID))

	rec = setTeammateAdmin(t, srv, admin, "00000000-0000-0000-0000-000000000000", map[string]any{"is_admin": true})
	assert.Equal(t, http.StatusNotFound, rec.Code)
}

func TestSetTeammateAdmin_MissingBodyRejected(t *testing.T) {
	srv, cleanup := setupTestServerFast(t)
	defer cleanup()

	team := createTestTeam(t, srv.DB, "Admins")
	admin := newTeamMember(t, srv, team, "admin@example.com", true)
	member := newTeamMember(t, srv, team, "member@example.com", false)

	rec := setTeammateAdmin(t, srv, admin, member.ID, map[string]any{})
	assert.Equal(t, http.StatusBadRequest, rec.Code)
}

func TestSetTeammateAdmin_LastAdminCannotBeRemoved(t *testing.T) {
	srv, cleanup := setupTestServerFast(t)
	defer cleanup()

	team := createTestTeam(t, srv.DB, "Admins")
	admin := newTeamMember(t, srv, team, "admin@example.com", true)
	newTeamMember(t, srv, team, "member@example.com", false)

	// Sole admin demoting themselves is refused.
	rec := setTeammateAdmin(t, srv, admin, admin.ID, map[string]any{"is_admin": false})
	assert.Equal(t, http.StatusBadRequest, rec.Code)
	assert.True(t, isAdminInDB(t, srv, admin.ID))
}

func TestSetTeammateAdmin_SelfDemoteAllowedWithAnotherAdmin(t *testing.T) {
	srv, cleanup := setupTestServerFast(t)
	defer cleanup()

	team := createTestTeam(t, srv.DB, "Admins")
	admin := newTeamMember(t, srv, team, "admin@example.com", true)
	admin2 := newTeamMember(t, srv, team, "admin2@example.com", true)

	rec := setTeammateAdmin(t, srv, admin, admin.ID, map[string]any{"is_admin": false})
	require.Equal(t, http.StatusOK, rec.Code, rec.Body.String())
	assert.False(t, isAdminInDB(t, srv, admin.ID))

	// admin2 is now the only admin and cannot step down.
	rec = setTeammateAdmin(t, srv, admin2, admin2.ID, map[string]any{"is_admin": false})
	assert.Equal(t, http.StatusBadRequest, rec.Code)
	assert.True(t, isAdminInDB(t, srv, admin2.ID))

	// The demoted admin can no longer change rights.
	rec = setTeammateAdmin(t, srv, admin, admin.ID, map[string]any{"is_admin": true})
	assert.Equal(t, http.StatusForbidden, rec.Code)
}
