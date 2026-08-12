// Copyright © 2026 Jalapeno Labs

// Core
import { describe, expect, it } from 'vitest'

// Misc
import {
  UrlTree,
  getCreativeConceptUrl,
  getOauthStartUrl,
  getUrlWithDestination,
  sanitizeDestination,
} from './urls'

describe('sanitizeDestination', () => {
  it('should keep a root-relative path', () => {
    expect(sanitizeDestination('/members')).toBe('/members')
  })

  it('should keep the query and hash of the destination', () => {
    expect(sanitizeDestination('/claim-invitation?token=abc123#welcome'))
      .toBe('/claim-invitation?token=abc123#welcome')
  })

  it('should return null when there is no destination', () => {
    expect(sanitizeDestination(null)).toBeNull()
    expect(sanitizeDestination(undefined)).toBeNull()
    expect(sanitizeDestination('')).toBeNull()
  })

  it('should reject absolute urls', () => {
    expect(sanitizeDestination('https://evil.example/steal')).toBeNull()
    expect(sanitizeDestination('javascript:alert(1)')).toBeNull()
  })

  it('should reject protocol-relative urls', () => {
    expect(sanitizeDestination('//evil.example')).toBeNull()
    expect(sanitizeDestination('/\\evil.example')).toBeNull()
  })

  it('should reject control characters browsers strip before resolving the url', () => {
    expect(sanitizeDestination('/\t/evil.example')).toBeNull()
    expect(sanitizeDestination('/members\n')).toBeNull()
  })

  it('should reject a path that does not start at the root', () => {
    expect(sanitizeDestination('members')).toBeNull()
    expect(sanitizeDestination(' /members')).toBeNull()
  })
})

describe('getUrlWithDestination', () => {
  it('should append the encoded destination', () => {
    expect(getUrlWithDestination(UrlTree.signIn, '/claim-invitation?token=abc123'))
      .toBe('/sign-in?next=%2Fclaim-invitation%3Ftoken%3Dabc123')
  })

  it('should return the bare url when there is no destination', () => {
    expect(getUrlWithDestination(UrlTree.signUp, null)).toBe('/sign-up')
  })

  it('should drop a destination that leaves the app', () => {
    expect(getUrlWithDestination(UrlTree.signIn, 'https://evil.example')).toBe('/sign-in')
  })
})

describe('getOauthStartUrl', () => {
  it('should point at the backend route for the provider', () => {
    expect(getOauthStartUrl('google', null)).toBe('/auth/oauth/google/start')
  })

  it('should carry the encoded destination through the provider round trip', () => {
    expect(getOauthStartUrl('google', '/creative-concepts?page=2'))
      .toBe('/auth/oauth/google/start?next=%2Fcreative-concepts%3Fpage%3D2')
  })

  it('should drop a destination that leaves the app', () => {
    expect(getOauthStartUrl('google', '//evil.example')).toBe('/auth/oauth/google/start')
  })
})

// The link factory a model scaffold writes above the `url-factories` anchor.
// Every generated page navigates through one, so the shape is worth pinning.
describe('getCreativeConceptUrl', () => {
  it('should fill the record id into the show route', () => {
    expect(getCreativeConceptUrl('9f8b7c6d')).toBe('/creative-concepts/9f8b7c6d')
  })

  it('should leave the rest of the url tree alone', () => {
    expect(UrlTree.creativeConcept).toBe('/creative-concepts/:creativeConceptId')
  })
})
