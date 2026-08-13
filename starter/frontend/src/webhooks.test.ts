// Copyright © 2026 Jalapeno Labs

import { describe, expect, it } from 'vitest'

import { deliveryStatusColor, isEventType, parseEventTypes } from './webhooks'

describe('isEventType', () => {
  it('should accept a snake-case model with a lifecycle action', () => {
    expect(isEventType('project.created')).toBe(true)
    expect(isEventType('applied_tag.destroyed')).toBe(true)
    expect(isEventType('widget2.updated')).toBe(true)
  })

  it('should refuse a typo in either half', () => {
    expect(isEventType('project.create')).toBe(false)
    expect(isEventType('project.deleted')).toBe(false)
    expect(isEventType('Project.created')).toBe(false)
    expect(isEventType('project-name.created')).toBe(false)
    expect(isEventType('_project.created')).toBe(false)
    expect(isEventType('project.created.twice')).toBe(false)
    expect(isEventType('')).toBe(false)
  })
})

describe('parseEventTypes', () => {
  it('should split on commas, spaces, and newlines alike', () => {
    expect(parseEventTypes('project.created, project.updated')).toEqual([
      'project.created',
      'project.updated',
    ])
    expect(parseEventTypes('project.created\nproject.updated')).toEqual([
      'project.created',
      'project.updated',
    ])
  })

  it('should drop blanks and duplicates, keeping the order typed', () => {
    expect(parseEventTypes('  project.updated , , project.created,project.updated ')).toEqual([
      'project.updated',
      'project.created',
    ])
    expect(parseEventTypes('   ')).toEqual([])
  })
})

describe('deliveryStatusColor', () => {
  it('should give every status a distinct chip color', () => {
    expect(deliveryStatusColor.pending).toBe('default')
    expect(deliveryStatusColor.delivered).toBe('success')
    expect(deliveryStatusColor.failed).toBe('warning')
    expect(deliveryStatusColor.dead).toBe('danger')
  })
})
